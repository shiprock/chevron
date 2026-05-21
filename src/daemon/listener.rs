//! Per-connection handler logic for the chevrond daemon.
//!
//! Each accepted `UnixStream` is handed to [`handle_connection`] on a
//! freshly-spawned thread. The handler reads a `HELLO` exchange, then loops
//! reading `STATUS`/`QUIT` requests until the client disconnects or sends an
//! invalid line.
//!
//! ## Why compute on the handler thread
//!
//! libgit2's status walk is the most expensive operation in the daemon, and
//! doing it on the state thread would serialise it across all clients. The
//! state thread is reserved for pure cache reads/writes; handler threads
//! consult the cache, and on miss do the compute themselves before storing
//! the result via [`StateMsg::Insert`].
//!
//! Two concurrent cold queries on the same workdir each compute (cost:
//! duplicated work, identical results — `RepoStatus::compute` is a pure
//! function of repo state). Phase 2 with the FS watcher will mostly
//! eliminate the cold path, so we don't optimise this here.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use git2::Repository;

use super::proto::{self, Request, Response};
use super::state::StateMsg;
use crate::segments::git::RepoStatus;

/// Per-message timeout on each connection. Generous because clients are
/// usually local and fast — this is just a guard against a hung peer.
const CONN_TIMEOUT: Duration = Duration::from_secs(5);

/// Read-line buffer cap. Larger inputs are treated as malformed (and the
/// connection is closed). Real STATUS lines are path-bounded; 8 KiB is
/// well past any sane filesystem path length.
const MAX_LINE_BYTES: usize = 8 * 1024;

/// Handle a single connection. Reads `HELLO`, then loops on `STATUS`/`QUIT`.
/// Returns silently on any error — the client will see the connection close
/// and fall back to inline compute.
///
/// Takes `conn` by value so the socket closes on return.
#[allow(clippy::needless_pass_by_value)]
pub fn handle_connection(conn: UnixStream, state_tx: &Sender<StateMsg>) {
    let _ = conn.set_read_timeout(Some(CONN_TIMEOUT));
    let _ = conn.set_write_timeout(Some(CONN_TIMEOUT));

    let mut reader = BufReader::new(&conn);
    let mut line = String::new();

    // HELLO handshake.
    if read_line(&mut reader, &mut line).is_err() {
        return;
    }
    match proto::decode_request(&line) {
        Ok(Request::Hello(v)) if v == proto::PROTO_VERSION => {
            if send_resp(&conn, &Response::Hello(v)).is_err() {
                return;
            }
        }
        Ok(Request::Hello(_)) => {
            let _ = send_resp(&conn, &Response::Err("unsupported version".into()));
            return;
        }
        Ok(_) => {
            let _ = send_resp(&conn, &Response::Err("expected HELLO".into()));
            return;
        }
        Err(_) => {
            let _ = send_resp(&conn, &Response::Err("malformed HELLO".into()));
            return;
        }
    }

    // Request loop.
    loop {
        line.clear();
        // Both EOF (Ok(0)) and a read error mean the client is gone; stop.
        match read_line(&mut reader, &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(req) = proto::decode_request(&line) else {
            if send_resp(&conn, &Response::Err("malformed request".into())).is_err() {
                return;
            }
            continue;
        };
        match req {
            Request::Quit => return,
            Request::Hello(_) => {
                if send_resp(&conn, &Response::Err("HELLO already exchanged".into())).is_err() {
                    return;
                }
            }
            Request::Status(path) => {
                let resp = handle_status(&path, state_tx);
                if send_resp(&conn, &resp).is_err() {
                    return;
                }
            }
        }
    }
}

fn read_line(reader: &mut BufReader<&UnixStream>, buf: &mut String) -> std::io::Result<usize> {
    use std::io::Read;
    // Cap line length so a malicious or buggy peer can't OOM us by streaming
    // bytes without a newline. `by_ref().take(N)` adapts the reader so
    // `read_until` reads at most N bytes — if it hits N without seeing '\n',
    // we treat that as a protocol violation.
    buf.clear();
    let mut tmp: Vec<u8> = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    let n = reader
        .by_ref()
        .take(MAX_LINE_BYTES as u64)
        .read_until(b'\n', &mut tmp)?;
    if n == MAX_LINE_BYTES && !tmp.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long",
        ));
    }
    buf.push_str(&String::from_utf8_lossy(&tmp));
    Ok(n)
}

fn send_resp(conn: &UnixStream, resp: &Response) -> std::io::Result<()> {
    // Build the full line (including trailing newline) and write in one
    // syscall so a reader using `read_line` always sees a complete line.
    // Two separate `write_all` calls would leave the door open for a reader
    // to observe the response without the newline.
    let mut line = proto::encode_response(resp);
    line.push('\n');
    let mut w = conn;
    w.write_all(line.as_bytes())?;
    Ok(())
}

/// Discover the repo at `cwd`, consult the cache, and compute on miss.
/// Returns the response to send back to the client.
#[must_use]
pub fn handle_status(cwd: &Path, state_tx: &Sender<StateMsg>) -> Response {
    let Ok(mut repo) = Repository::discover(cwd) else {
        return Response::Status(None);
    };

    // Bare repos have no workdir, so we can't compute the git segment.
    // Match the inline path's behaviour by reporting NONE.
    let Some(workdir) = repo.workdir().and_then(|p| p.canonicalize().ok()) else {
        return Response::Status(None);
    };
    // `repo.path()` returns the actual gitdir — handles submodules and
    // git-worktrees where `.git` is a file pointing elsewhere. This is
    // what we hand to the watcher.
    let git_dir = repo.path().to_path_buf();

    // Cache lookup.
    let (reply_tx, reply_rx) = mpsc::channel();
    if state_tx
        .send(StateMsg::Get {
            workdir: workdir.clone(),
            reply: reply_tx,
        })
        .is_err()
    {
        // State thread is gone; degrade gracefully by computing here. The
        // result won't be cached, but the client still gets a correct answer.
        let status = RepoStatus::compute(&mut repo);
        return Response::Status(Some(status));
    }

    // Short timeout: the state thread is local and synchronous; if it's not
    // responding in 100 ms something is badly wrong. Treat as miss.
    let hit = reply_rx
        .recv_timeout(Duration::from_millis(100))
        .ok()
        .flatten();
    if let Some(status) = hit {
        return Response::Status(Some(status));
    }

    // Miss: compute, store, return.
    let status = RepoStatus::compute(&mut repo);
    let _ = state_tx.send(StateMsg::Insert {
        workdir,
        git_dir,
        status: status.clone(),
        computed_at: Instant::now(),
    });
    Response::Status(Some(status))
}

/// Accept loop. Blocks for the lifetime of the daemon, spawning a fresh
/// thread per connection. Returns when the listener is closed or accept
/// fails fatally.
pub fn serve_loop(listener: &UnixListener, state_tx: &Sender<StateMsg>) {
    for conn in listener.incoming() {
        match conn {
            Ok(conn) => {
                let tx = state_tx.clone();
                let spawn_result = std::thread::Builder::new()
                    .name("chevrond-conn".into())
                    .spawn(move || handle_connection(conn, &tx));
                if let Err(e) = spawn_result {
                    eprintln!("chevrond: failed to spawn handler thread: {e}");
                }
            }
            Err(e) => {
                // Transient accept errors are logged but not fatal; the
                // listener itself is still healthy.
                eprintln!("chevrond: accept error: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state;
    use std::time::Duration;

    /// Drive a `UnixStream` pair: spawn [`handle_connection`] on one end,
    /// return a `Client` view of the other for the test to talk to.
    fn spawn_handler() -> (Client, Sender<StateMsg>, std::thread::JoinHandle<()>) {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (state_tx, _state_join) = state::spawn(state::TTL).unwrap();
        let tx_for_handler = state_tx.clone();
        let join = std::thread::spawn(move || handle_connection(b, &tx_for_handler));
        (Client::new(a), state_tx, join)
    }

    /// Per-test client that owns the stream plus a persistent `BufReader`.
    /// Holding the reader across calls prevents the second response from
    /// being read as part of the first (`BufReader` pre-fills its buffer).
    struct Client {
        stream: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl Client {
        fn new(stream: UnixStream) -> Self {
            let reader = BufReader::new(stream.try_clone().unwrap());
            Self { stream, reader }
        }

        fn send(&self, line: &str) {
            let mut w = &self.stream;
            w.write_all(line.as_bytes()).unwrap();
            w.write_all(b"\n").unwrap();
        }

        fn recv(&mut self) -> String {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            line
        }

        fn close(self) {
            drop(self.reader);
            drop(self.stream);
        }
    }

    #[test]
    fn handshake_round_trips() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 1");
        let resp = client.recv();
        assert!(resp.starts_with("HELLO 1"), "got: {resp:?}");
        client.send("QUIT");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn unsupported_version_returns_err_and_closes() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 999");
        let resp = client.recv();
        assert!(resp.starts_with("ERR "), "got: {resp:?}");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn missing_hello_first_returns_err_and_closes() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("STATUS /tmp");
        let resp = client.recv();
        assert!(resp.starts_with("ERR "), "got: {resp:?}");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn malformed_hello_returns_err_and_closes() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO abc");
        let resp = client.recv();
        assert!(resp.starts_with("ERR "), "got: {resp:?}");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn status_on_non_repo_returns_none() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 1");
        let _ = client.recv();
        let tmp = tempfile::TempDir::new().unwrap();
        client.send(&format!("STATUS {}", tmp.path().display()));
        let resp = client.recv();
        assert!(resp.starts_with("NONE"), "got: {resp:?}");
        client.send("QUIT");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn status_on_repo_returns_ok() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 1");
        let _ = client.recv();
        let tmp = tempfile::TempDir::new().unwrap();
        crate::segments::testutil::init_repo(tmp.path());
        client.send(&format!("STATUS {}", tmp.path().display()));
        let resp = client.recv();
        assert!(resp.starts_with("OK "), "got: {resp:?}");
        // Parsing the response back should yield a valid RepoStatus.
        let parsed = proto::decode_response(&resp).unwrap();
        match parsed {
            Response::Status(Some(s)) => {
                assert!(!s.repo_name.is_empty());
                assert!(!s.branch.is_empty());
            }
            other => panic!("unexpected response: {other:?}"),
        }
        client.send("QUIT");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn second_status_hits_cache() {
        // After the first STATUS computes and caches, the second STATUS for
        // the same workdir should be served from cache. We can't easily
        // observe "did we touch libgit2" from outside, but we can verify the
        // response is identical byte-for-byte.
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 1");
        let _ = client.recv();
        let tmp = tempfile::TempDir::new().unwrap();
        crate::segments::testutil::init_repo(tmp.path());
        let path_arg = format!("STATUS {}", tmp.path().display());
        client.send(&path_arg);
        let first = client.recv();
        client.send(&path_arg);
        let second = client.recv();
        assert_eq!(first, second);
        client.send("QUIT");
        client.close();
        join.join().unwrap();
    }

    #[test]
    fn malformed_request_after_hello_returns_err_and_continues() {
        let (mut client, _tx, join) = spawn_handler();
        client.send("HELLO 1");
        let _ = client.recv();
        client.send("FOOBAR garbage");
        let err = client.recv();
        assert!(err.starts_with("ERR "), "got: {err:?}");
        // Connection should still be usable for a follow-up request.
        let tmp = tempfile::TempDir::new().unwrap();
        client.send(&format!("STATUS {}", tmp.path().display()));
        let resp = client.recv();
        assert!(resp.starts_with("NONE"), "got: {resp:?}");
        client.send("QUIT");
        client.close();
        join.join().unwrap();
    }
}
