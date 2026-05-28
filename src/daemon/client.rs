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

/// Per-syscall timeout for fire-and-forget lifecycle events (`CMD_START`,
/// `CMD_END`). Longer than `QUERY_TIMEOUT` because losing a history
/// event silently is worse than a 25 ms blip on the keystroke hot path
/// — but still bounded so a hung daemon can't pin preexec indefinitely.
const PUBLISH_TIMEOUT: Duration = Duration::from_millis(25);

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

/// Ask the running daemon for its binary / proto / schema versions.
/// Returns `None` if the daemon isn't reachable or replies with an
/// unexpected response — callers should treat that as "no daemon"
/// rather than an error.
#[must_use]
pub fn try_version() -> Option<proto::DaemonVersion> {
    let conn = UnixStream::connect(paths::socket_path()).ok()?;
    conn.set_read_timeout(Some(QUERY_TIMEOUT)).ok()?;
    conn.set_write_timeout(Some(QUERY_TIMEOUT)).ok()?;

    let mut reader = BufReader::new(&conn);
    let mut line = String::new();

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

    line.clear();
    write_line(&conn, &proto::encode_request(&proto::Request::Version)).ok()?;
    reader.read_line(&mut line).ok()?;
    match proto::decode_response(&line).ok()? {
        proto::Response::Version(v) => Some(v),
        _ => None,
    }
}

/// `chevron daemon version`: print the running daemon's versions and
/// compare against the CLI's own `CARGO_PKG_VERSION`. Returns 0 if
/// everything matches or if no daemon is running (informational
/// — not a failure mode); 1 on a version mismatch the user should
/// resolve by restarting the daemon.
#[must_use]
pub fn print_version() -> i32 {
    let cli_binary = env!("CARGO_PKG_VERSION");
    let cli_proto = proto::PROTO_VERSION;

    println!("chevron cli:    {cli_binary} (proto={cli_proto})");

    let Some(v) = try_version() else {
        println!("chevron daemon: not running");
        return 0;
    };
    println!(
        "chevron daemon: {} (proto={}, schema={})",
        v.binary, v.proto, v.schema
    );

    let mut mismatch = false;
    if v.binary != cli_binary {
        eprintln!();
        eprintln!(
            "WARNING: daemon binary ({}) differs from CLI ({}).",
            v.binary, cli_binary
        );
        eprintln!("Run `chevron daemon stop && chevron daemon start` to upgrade.");
        mismatch = true;
    }
    if v.proto != cli_proto {
        eprintln!();
        eprintln!(
            "WARNING: daemon proto ({}) differs from CLI ({}) — protocol drift.",
            v.proto, cli_proto
        );
        mismatch = true;
    }
    // Schema mismatch isn't necessarily a problem (forward-compat),
    // but it's worth flagging since most schema bumps come with
    // CLI-side reader changes too.
    i32::from(mismatch)
}

/// Publish a lifecycle event (`CMD_START` or `CMD_END`) to the running
/// daemon. Returns `true` if the daemon ack'd within [`PUBLISH_TIMEOUT`],
/// `false` on any failure. The shell hooks treat both as success — a
/// lost event just means one missing history row.
///
/// Why HELLO + read response + send + read ACK rather than write-and-close:
/// without reading the daemon's HELLO response, a client closing the
/// socket immediately can trip the daemon's `send_resp` into `EPIPE` and
/// short-circuit the per-connection loop before it processes our actual
/// event. The two round-trips are ~100 µs on local UDS — well under the
/// [`PUBLISH_TIMEOUT`] budget.
#[must_use]
pub fn try_publish_event(req: &proto::Request) -> bool {
    debug_assert!(
        matches!(req, proto::Request::CmdStart(_) | proto::Request::CmdEnd(_)),
        "try_publish_event is for lifecycle events only"
    );
    let Ok(conn) = UnixStream::connect(paths::socket_path()) else {
        return false;
    };
    if conn.set_read_timeout(Some(PUBLISH_TIMEOUT)).is_err()
        || conn.set_write_timeout(Some(PUBLISH_TIMEOUT)).is_err()
    {
        return false;
    }
    let mut reader = BufReader::new(&conn);
    let mut line = String::new();
    if write_line(
        &conn,
        &proto::encode_request(&proto::Request::Hello(proto::PROTO_VERSION)),
    )
    .is_err()
    {
        return false;
    }
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    match proto::decode_response(&line) {
        Ok(proto::Response::Hello(v)) if v == proto::PROTO_VERSION => {}
        _ => return false,
    }

    line.clear();
    if write_line(&conn, &proto::encode_request(req)).is_err() {
        return false;
    }
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    matches!(proto::decode_response(&line), Ok(proto::Response::Ack))
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

    /// Test timeout — generous enough for sandbox builds where hundreds
    /// of parallel tests compete for CPU. The production `QUERY_TIMEOUT`
    /// (10 ms) is deliberately tight; tests must not depend on it.
    const TEST_TIMEOUT: Duration = Duration::from_millis(500);

    /// Like [`try_version`] but with [`TEST_TIMEOUT`].
    fn try_version_patient() -> Option<proto::DaemonVersion> {
        let conn = UnixStream::connect(paths::socket_path()).ok()?;
        conn.set_read_timeout(Some(TEST_TIMEOUT)).ok()?;
        conn.set_write_timeout(Some(TEST_TIMEOUT)).ok()?;
        let mut reader = BufReader::new(&conn);
        let mut line = String::new();
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
        line.clear();
        write_line(&conn, &proto::encode_request(&proto::Request::Version)).ok()?;
        reader.read_line(&mut line).ok()?;
        match proto::decode_response(&line).ok()? {
            proto::Response::Version(v) => Some(v),
            _ => None,
        }
    }

    /// Like [`try_query`] but with [`TEST_TIMEOUT`].
    fn try_query_patient(cwd: &Path) -> Option<RepoStatus> {
        let conn = UnixStream::connect(paths::socket_path()).ok()?;
        conn.set_read_timeout(Some(TEST_TIMEOUT)).ok()?;
        conn.set_write_timeout(Some(TEST_TIMEOUT)).ok()?;
        let mut reader = BufReader::new(&conn);
        let mut line = String::new();
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

    /// Like [`try_publish_event`] but with [`TEST_TIMEOUT`].
    fn try_publish_event_patient(req: &proto::Request) -> bool {
        let Ok(conn) = UnixStream::connect(paths::socket_path()) else {
            return false;
        };
        if conn.set_read_timeout(Some(TEST_TIMEOUT)).is_err()
            || conn.set_write_timeout(Some(TEST_TIMEOUT)).is_err()
        {
            return false;
        }
        let mut reader = BufReader::new(&conn);
        let mut line = String::new();
        if write_line(
            &conn,
            &proto::encode_request(&proto::Request::Hello(proto::PROTO_VERSION)),
        )
        .is_err()
        {
            return false;
        }
        if reader.read_line(&mut line).is_err() {
            return false;
        }
        match proto::decode_response(&line) {
            Ok(proto::Response::Hello(v)) if v == proto::PROTO_VERSION => {}
            _ => return false,
        }
        line.clear();
        if write_line(&conn, &proto::encode_request(req)).is_err() {
            return false;
        }
        if reader.read_line(&mut line).is_err() {
            return false;
        }
        matches!(proto::decode_response(&line), Ok(proto::Response::Ack))
    }

    /// Spin up a one-off daemon bound to a tempdir socket and return its
    /// socket path. Sets `CHEVRON_SOCKET_DIR` so `try_query` picks it up.
    /// Returns the dir so it lives as long as the test.
    fn spawn_daemon() -> TempDir {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir.path()) };
        let sock = dir.path().join("chevrond.sock");
        let listener_sock = UnixListener::bind(&sock).unwrap();
        let db = state::open_db(dir.path()).unwrap();
        let (state_tx, _state_join) = state::spawn(state::TTL, db).unwrap();
        std::thread::spawn(move || listener::serve_loop(&listener_sock, &state_tx));
        // Block until the daemon is ready to serve requests.
        for _ in 0..10 {
            if try_version_patient().is_some() {
                return dir;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("spawn_daemon: daemon did not become ready");
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
        let status = try_query_patient(&cwd).expect("expected daemon to return status");
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

    #[test]
    #[serial]
    fn try_version_returns_none_when_no_daemon() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir.path()) };
        assert!(try_version().is_none());
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn try_version_reports_daemon_versions() {
        // Against a live daemon, the VERSION query should report this
        // crate's CARGO_PKG_VERSION, the current proto, and the
        // current schema version. The CLI's print_version() uses the
        // same source of truth, so a clean upgrade-restart always
        // shows matching binaries.
        let _dir = spawn_daemon();
        let v = try_version_patient().expect("expected a VERSION response");
        assert_eq!(v.binary, env!("CARGO_PKG_VERSION"));
        assert_eq!(v.proto, proto::PROTO_VERSION);
        // Schema is a string; just assert it parses as a positive
        // integer rather than hardcoding "2" (which would have to
        // be updated on every schema bump).
        let schema_n: u32 = v.schema.parse().expect("schema should be numeric");
        assert!(schema_n >= 1);
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn try_publish_event_returns_false_when_no_daemon() {
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir.path()) };
        let req = proto::Request::CmdStart(proto::CmdStartEvent {
            id: "id-1".into(),
            session_id: "s".into(),
            hostname: "h".into(),
            cwd: dir.path().to_path_buf(),
            cmd: "ls".into(),
            started_at_ms: 1,
        });
        assert!(!try_publish_event(&req));
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn try_publish_event_persists_cmd_start_and_end() {
        // End-to-end: publish a CmdStart + CmdEnd via the public client,
        // then open the on-disk DB and verify the row landed with the
        // correct completion fields. This is the integration that proves
        // the wire layer, the actor's SQLite path, and the lifecycle
        // helpers compose correctly.
        let dir = spawn_daemon();

        let start = proto::CmdStartEvent {
            id: "test-cmd-1".to_string(),
            session_id: "test-sess".to_string(),
            hostname: "test-host".to_string(),
            cwd: std::path::PathBuf::from("/tmp/test"),
            cmd: "echo 'hi  there'".to_string(),
            started_at_ms: 100,
        };
        assert!(try_publish_event_patient(&proto::Request::CmdStart(
            start.clone()
        )));

        let end = proto::CmdEndEvent {
            id: "test-cmd-1".to_string(),
            finished_at_ms: 250,
            duration_ms: 150,
            exit_status: 0,
            output_bytes: None,
            output_truncated: None,
        };
        assert!(try_publish_event_patient(&proto::Request::CmdEnd(end)));

        // The listener ACKs lifecycle events as soon as they're queued
        // for the state actor — actual SQLite commit happens
        // asynchronously on the state thread. Flush by issuing a
        // STATUS request: the actor processes messages in mpsc order,
        // so once the STATUS reply lands, the earlier CmdStart and
        // CmdEnd have been processed. More robust than a sleep under
        // load (the original Phase 1 test was flaky in 5x-stress
        // pre-push runs).
        let _ = try_query_patient(dir.path());

        // Use a fresh read-only connection so the actor's writer-side
        // WAL state still flushes our row to readers. WAL allows
        // concurrent reads against an open writer.
        let conn = rusqlite::Connection::open(dir.path().join("commands.db")).unwrap();
        let (cmd, finished_at, exit_status): (String, i64, i64) = conn
            .query_row(
                "SELECT cmd, finished_at, exit_status FROM commands WHERE id = ?1",
                ["test-cmd-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cmd, "echo 'hi  there'");
        assert_eq!(finished_at, 250);
        assert_eq!(exit_status, 0);

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

        let daemon = try_query_patient(&cwd).expect("daemon should return a status");
        let mut inline_repo = git2::Repository::discover(&cwd).unwrap();
        let inline = RepoStatus::compute(&mut inline_repo);

        assert_eq!(
            daemon, inline,
            "daemon-served RepoStatus must equal inline-computed value"
        );
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }
}
