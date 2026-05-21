//! Daemon client — try to query a running chevrond, and self-bootstrap
//! the daemon on miss so subsequent prompts hit a warm cache.
//!
//! Any error path — daemon not running, slow reply, malformed response —
//! returns `None`. Callers then fall back to inline `RepoStatus::compute`,
//! so the daemon is always purely additive.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{lifecycle, paths, proto};
use crate::segments::git::RepoStatus;

/// Per-syscall timeout on the daemon's socket. The local daemon's actual
/// reply latency is well under 1 ms; this is just a guard against a hung
/// peer (e.g. the state thread deadlocked). On timeout we fall back to
/// inline compute — the shell still gets a prompt.
const QUERY_TIMEOUT: Duration = Duration::from_millis(10);

/// Ask the running daemon for `cwd`'s status. Returns `None` for any
/// failure so the caller can transparently degrade to inline compute.
///
/// Note: a `NONE` response from the daemon (no repo at this path) also
/// returns `None`. The inline retry that follows will reach the same
/// conclusion — accepting one extra ~50 µs `Repository::discover` keeps
/// this function's contract single-meaning.
#[must_use]
pub fn try_query(cwd: &Path) -> Option<RepoStatus> {
    let conn = UnixStream::connect(paths::socket_path()).ok()?;
    conn.set_read_timeout(Some(QUERY_TIMEOUT)).ok()?;
    conn.set_write_timeout(Some(QUERY_TIMEOUT)).ok()?;

    let mut reader = BufReader::new(&conn);
    let mut line = String::new();

    // HELLO handshake.
    write_line(
        &conn,
        &proto::encode_request(&proto::Request::Hello(proto::PROTO_VERSION)),
    )
    .ok()?;
    reader.read_line(&mut line).ok()?;
    match proto::decode_response(&line).ok()? {
        proto::Response::Hello(v) if v == proto::PROTO_VERSION => {}
        _ => return None,
    }

    // STATUS request.
    line.clear();
    write_line(
        &conn,
        &proto::encode_request(&proto::Request::Status(cwd.to_path_buf())),
    )
    .ok()?;
    reader.read_line(&mut line).ok()?;

    match proto::decode_response(&line).ok()? {
        proto::Response::Status(Some(s)) => Some(s),
        _ => None,
    }
}

fn write_line(mut conn: &UnixStream, line: &str) -> std::io::Result<()> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    conn.write_all(buf.as_bytes())
}

/// Attempt to spawn a detached daemon. Best-effort: any failure here just
/// means the *next* prompt also goes through the inline path. Multiple
/// concurrent callers are deduped via [`lifecycle::try_lock_exclusive`]
/// on `chevrond.lock` — only the first one in this critical section forks.
///
/// The lost-race daemons (multiple shells starting in parallel before any
/// have spawned) still race for the same lock once they run
/// [`lifecycle::serve`], so at most one daemon ends up bound to the socket.
pub fn try_spawn_async() {
    let dir = paths::socket_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Briefly hold the spawn lock to keep N concurrent prompts from each
    // forking. Dropped after spawn returns — the daemon will re-acquire
    // it once it reaches lifecycle::serve.
    let Ok(_lock) = lifecycle::try_lock_exclusive(&paths::lock_path()) else {
        return;
    };
    let _ = spawn_detached();
}

fn spawn_detached() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid() is async-signal-safe and the only effect we want in
    // the forked child — it detaches us from the controlling terminal so
    // the daemon survives the spawning shell's exit.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{listener, state};
    use serial_test::serial;
    use std::os::unix::net::UnixListener;
    use tempfile::TempDir;

    /// Spin up a one-off daemon bound to a tempdir socket and return its
    /// socket path. Sets `CHEVRON_SOCKET_DIR` so `try_query` picks it up.
    /// Returns the dir so it lives as long as the test.
    fn spawn_daemon() -> TempDir {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir.path()) };
        let sock = dir.path().join("chevrond.sock");
        let listener_sock = UnixListener::bind(&sock).unwrap();
        let (state_tx, _state_join) = state::spawn(state::TTL).unwrap();
        std::thread::spawn(move || listener::serve_loop(&listener_sock, &state_tx));
        dir
    }

    #[test]
    #[serial]
    fn try_query_returns_none_when_no_daemon() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir.path()) };
        assert!(try_query(dir.path()).is_none());
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn try_query_against_live_daemon_returns_repo_status() {
        let _dir = spawn_daemon();
        let repo_tmp = TempDir::new().unwrap();
        crate::segments::testutil::init_repo(repo_tmp.path());
        let cwd = repo_tmp.path().canonicalize().unwrap();
        let status = try_query(&cwd).expect("expected daemon to return status");
        assert!(!status.branch.is_empty());
        assert!(!status.repo_name.is_empty());
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn try_query_returns_none_for_non_repo_path() {
        let _dir = spawn_daemon();
        let other = TempDir::new().unwrap();
        assert!(try_query(other.path()).is_none());
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    /// **Load-bearing invariant**: the daemon must produce a `RepoStatus`
    /// byte-equal to what inline compute would produce for the same repo
    /// state. If this test ever fails, the daemon-fast-path and
    /// inline-fallback path have diverged — a user could see different
    /// prompts depending on whether the daemon happens to be running.
    #[test]
    #[serial]
    fn invariant_daemon_matches_inline() {
        let _dir = spawn_daemon();

        // Build a repo with at least one of every signal we render:
        // an untracked file, a staged file, a modified-since-staged change.
        let repo_tmp = TempDir::new().unwrap();
        let repo = crate::segments::testutil::init_repo(repo_tmp.path());
        std::fs::write(repo_tmp.path().join("untracked.txt"), "x").unwrap();
        let staged_path = repo_tmp.path().join("staged.txt");
        std::fs::write(&staged_path, "v1").unwrap();
        {
            let mut idx = repo.index().unwrap();
            idx.add_path(std::path::Path::new("staged.txt")).unwrap();
            idx.write().unwrap();
        }

        let cwd = repo_tmp.path().canonicalize().unwrap();

        let daemon = try_query(&cwd).expect("daemon should return a status");
        let mut inline_repo = git2::Repository::discover(&cwd).unwrap();
        let inline = RepoStatus::compute(&mut inline_repo);

        assert_eq!(
            daemon, inline,
            "daemon-served RepoStatus must equal inline-computed value"
        );
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }
}
