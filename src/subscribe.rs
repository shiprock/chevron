//! `chevron subscribe` — subscriber-relay helper for live prompt
//! segments (chevron-1yn.3 Phase 3).
//!
//! Opens a persistent UDS connection to chevrond, completes the HELLO
//! handshake, sends `SUBSCRIBE`, then echoes each incoming `EVENT`
//! line to stdout verbatim. `PING` keepalives are consumed silently
//! so the shell-side `zle -F` handler only wakes for real events.
//!
//! Lifecycle is bounded by the parent process: when the spawning
//! shell exits, this helper gets SIGHUP (or its parent-pid changes)
//! and exits cleanly. The daemon notices the socket close on its
//! next broadcast attempt and prunes the subscriber.
//!
//! ## Reconnect (chevron-1mh)
//!
//! A daemon restart (`chevron daemon stop`/`start`, crash + respawn,
//! upgrade) closes the subscriber socket. Rather than exit and leave the
//! shell dark until a new session, the helper reconnects with bounded
//! exponential backoff: the budget resets on every successful
//! subscription, so a flapping daemon recovers each time, while a
//! permanently-dead one is abandoned after the budget is spent so it
//! doesn't pin the process. The shell's `zle -F` pipe stays open across
//! the whole dance — it only sees EOF if the helper finally gives up.
//!
//! ## Why a separate process
//!
//! The shell itself can't hold a long-lived socket cleanly — its
//! event loop is wired to `zle -F` on file descriptors, not to
//! stateful protocols. Putting the socket-talking in a dedicated
//! child lets us:
//!
//! - keep the shell init minimal (one `exec {fd}< <(...)` + `zle -F`)
//! - rely on the kernel for lifecycle bookkeeping (child dies with
//!   parent via SIGHUP)
//! - filter PING noise out at the helper layer so the shell only
//!   sees real EVENT lines

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::daemon::{paths, proto};

/// Generous connect timeout — chevrond is local and fast, but we
/// shouldn't pin the shell init on a hung socket. If the daemon
/// isn't responding within a second something's badly wrong; exit
/// quietly and let the shell run without live updates.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Maximum consecutive failed reconnect attempts (since the last
/// successful subscription) before the helper gives up and exits, so a
/// permanently-dead daemon doesn't pin the process forever.
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// How one connect → handshake → relay cycle ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Never established a working subscription this cycle (connect or
    /// handshake failed). Retryable — but at startup it means "no daemon".
    ConnectFailed,
    /// Was relaying, then the daemon/connection dropped (EOF or socket
    /// error). Retryable — the daemon is likely mid-restart.
    Disconnected,
    /// Our stdout (the shell's read end) closed: the parent shell is gone,
    /// so there's nothing left to relay to. Terminal.
    ShellGone,
}

/// What the reconnect loop should do after one cycle.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Stop, returning this process exit code.
    Exit(i32),
    /// Reconnect, optionally sleeping first (`None` = retry immediately).
    Reconnect(Option<Duration>),
}

/// Dispatch a `chevron subscribe …` invocation. Exit codes:
///
/// - 0: clean exit (shell gone, or the daemon stayed gone past the bounded
///   reconnect budget)
/// - 1: argument error
/// - 2: initial connect/handshake failure (no daemon at startup)
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let cwd_filter = match parse_args(args) {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("chevron subscribe: {msg}");
            eprintln!("Usage: chevron subscribe [--cwd PATH]");
            return 1;
        }
    };

    let mut ever_subscribed = false;
    let mut failed_attempts = 0u32;
    loop {
        let outcome = connect_and_relay(cwd_filter.clone());
        match next_action(&outcome, &mut ever_subscribed, &mut failed_attempts) {
            Action::Exit(code) => {
                if code == 2 {
                    eprintln!("chevron subscribe: daemon not running");
                }
                return code;
            }
            Action::Reconnect(delay) => {
                if let Some(d) = delay {
                    std::thread::sleep(d);
                }
            }
        }
    }
}

/// Decide what to do after a connect/relay cycle. Pure (modulo the two
/// counters it threads) so the reconnect policy is unit-testable without a
/// daemon. `ever_subscribed` latches once a subscription succeeded;
/// `failed_attempts` counts consecutive connect failures since the last
/// success and is reset by a real disconnect.
fn next_action(outcome: &Outcome, ever_subscribed: &mut bool, failed_attempts: &mut u32) -> Action {
    match outcome {
        Outcome::ShellGone => Action::Exit(0),
        Outcome::Disconnected => {
            // A real session ended (daemon went away). Treat the next
            // connect as a fresh restart: reset the budget, retry now.
            *ever_subscribed = true;
            *failed_attempts = 0;
            Action::Reconnect(None)
        }
        Outcome::ConnectFailed => {
            if !*ever_subscribed {
                // No daemon at startup — preserve the historic contract
                // (and the shell's graceful fallback) by exiting 2.
                return Action::Exit(2);
            }
            *failed_attempts += 1;
            match backoff_delay(*failed_attempts) {
                Some(d) => Action::Reconnect(Some(d)),
                None => Action::Exit(0),
            }
        }
    }
}

/// Exponential backoff (100 ms base, doubling, capped at 2 s) for reconnect
/// attempt `n` (1-based). Returns `None` once the attempt count exceeds the
/// budget, signalling the caller to give up.
fn backoff_delay(attempt: u32) -> Option<Duration> {
    const BASE_MS: u64 = 100;
    const CAP_MS: u64 = 2000;
    if attempt == 0 || attempt > MAX_RECONNECT_ATTEMPTS {
        return None;
    }
    let shift = (attempt - 1).min(16);
    let ms = BASE_MS.saturating_mul(1u64 << shift).min(CAP_MS);
    Some(Duration::from_millis(ms))
}

/// One connect → HELLO → SUBSCRIBE → relay cycle. Returns when the
/// connection ends, classifying why so [`run`] can decide whether to
/// reconnect. Stays quiet on failure (the caller owns user-facing
/// messaging) so reconnect attempts don't spam stderr.
fn connect_and_relay(cwd_filter: Option<PathBuf>) -> Outcome {
    let Ok(conn) = UnixStream::connect(paths::socket_path()) else {
        return Outcome::ConnectFailed;
    };
    let _ = conn.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = conn.set_write_timeout(Some(CONNECT_TIMEOUT));

    let mut reader = BufReader::new(&conn);
    let mut line = String::new();

    // HELLO handshake.
    if write_line(
        &conn,
        &proto::encode_request(&proto::Request::Hello(proto::PROTO_VERSION)),
    )
    .is_err()
    {
        return Outcome::ConnectFailed;
    }
    if reader.read_line(&mut line).is_err() {
        return Outcome::ConnectFailed;
    }
    match proto::decode_response(&line) {
        Ok(proto::Response::Hello(v)) if v == proto::PROTO_VERSION => {}
        _ => return Outcome::ConnectFailed,
    }

    // SUBSCRIBE.
    line.clear();
    if write_line(
        &conn,
        &proto::encode_request(&proto::Request::Subscribe(proto::SubscribeSpec {
            cwd: cwd_filter,
        })),
    )
    .is_err()
    {
        return Outcome::ConnectFailed;
    }
    if reader.read_line(&mut line).is_err() {
        return Outcome::ConnectFailed;
    }
    match proto::decode_response(&line) {
        Ok(proto::Response::Ack) => {}
        _ => return Outcome::ConnectFailed,
    }

    // Now in subscriber-relay mode. The daemon's PING heartbeats can
    // arrive minutes apart, so we drop the per-read timeout — we want
    // to block on the socket indefinitely until something arrives.
    let _ = conn.set_read_timeout(None);

    let mut stdout = std::io::stdout().lock();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // EOF or socket error: daemon closed, shutting down, or any
            // I/O failure. A real session ended — ask to reconnect.
            Ok(0) | Err(_) => return Outcome::Disconnected,
            Ok(_) => {}
        }
        // Parse the line. We only forward EVENT lines to stdout;
        // PING/ERR/HELLO are consumed silently. This way the shell's
        // zle -F handler wakes only on real state changes — keepalive
        // noise doesn't trigger spurious redraws.
        let Ok(resp) = proto::decode_response(&line) else {
            // Malformed line — log to stderr (visible only when the
            // helper isn't backgrounded) and keep going. The protocol
            // is forward-compatible, so this should be rare.
            eprintln!("chevron subscribe: malformed line: {line:?}");
            continue;
        };
        if matches!(resp, proto::Response::Event(_)) {
            // Re-emit the canonical encoded form (trims any leading
            // whitespace / spurious CR that BufReader preserves).
            if writeln!(stdout, "{}", proto::encode_response(&resp)).is_err() {
                // Shell's read end of our stdout closed — parent gone.
                return Outcome::ShellGone;
            }
            if stdout.flush().is_err() {
                return Outcome::ShellGone;
            }
        }
        // Else: PING / unexpected — discard.
    }
}

fn parse_args(args: &[String]) -> Result<Option<PathBuf>, String> {
    let mut cwd: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cwd" => {
                let Some(val) = args.get(i + 1) else {
                    return Err("--cwd requires a path".to_string());
                };
                cwd = Some(PathBuf::from(val));
                i += 2;
            }
            "-h" | "--help" => {
                return Err("help".to_string());
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }
    Ok(cwd)
}

fn write_line(mut conn: &UnixStream, line: &str) -> std::io::Result<()> {
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    conn.write_all(buf.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn args_default_no_cwd() {
        assert_eq!(parse_args(&[]).unwrap(), None);
    }

    #[test]
    fn args_with_cwd() {
        let args = vec!["--cwd".to_string(), "/x".to_string()];
        assert_eq!(parse_args(&args).unwrap(), Some(PathBuf::from("/x")));
    }

    #[test]
    fn args_missing_cwd_value_errors() {
        let args = vec!["--cwd".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn args_unknown_flag_errors() {
        let args = vec!["--mystery".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    #[serial]
    fn run_exits_2_when_no_daemon() {
        // Point at a definitely-missing socket dir to avoid any real
        // daemon on the dev box. The initial connect fails before any
        // subscription, so run() must exit 2 (not enter the reconnect
        // loop).
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", tmp.path()) };
        assert_eq!(run(&[]), 2);
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    fn backoff_is_bounded_monotonic_and_capped() {
        assert_eq!(backoff_delay(0), None);
        assert_eq!(backoff_delay(1), Some(Duration::from_millis(100)));
        assert_eq!(backoff_delay(2), Some(Duration::from_millis(200)));
        assert_eq!(backoff_delay(5), Some(Duration::from_millis(1600)));
        // Capped at 2 s.
        assert_eq!(backoff_delay(6), Some(Duration::from_millis(2000)));
        assert_eq!(
            backoff_delay(MAX_RECONNECT_ATTEMPTS),
            Some(Duration::from_millis(2000))
        );
        // Budget spent past the cap.
        assert_eq!(backoff_delay(MAX_RECONNECT_ATTEMPTS + 1), None);
        // Non-decreasing across the budget.
        for n in 1..MAX_RECONNECT_ATTEMPTS {
            assert!(backoff_delay(n).unwrap() <= backoff_delay(n + 1).unwrap());
        }
    }

    #[test]
    fn policy_shell_gone_exits_zero() {
        let (mut ever, mut fails) = (true, 3);
        assert_eq!(
            next_action(&Outcome::ShellGone, &mut ever, &mut fails),
            Action::Exit(0)
        );
    }

    #[test]
    fn policy_initial_connect_failure_exits_two() {
        let (mut ever, mut fails) = (false, 0);
        assert_eq!(
            next_action(&Outcome::ConnectFailed, &mut ever, &mut fails),
            Action::Exit(2)
        );
    }

    #[test]
    fn policy_disconnect_resets_budget_and_retries_now() {
        let (mut ever, mut fails) = (false, 5);
        assert_eq!(
            next_action(&Outcome::Disconnected, &mut ever, &mut fails),
            Action::Reconnect(None)
        );
        assert!(ever, "a disconnect implies we had subscribed");
        assert_eq!(fails, 0, "a real session resets the backoff budget");
    }

    #[test]
    fn policy_connect_failure_after_session_backs_off_then_gives_up() {
        let (mut ever, mut fails) = (true, 0);
        // Climbs through the whole backoff budget...
        for _ in 0..MAX_RECONNECT_ATTEMPTS {
            assert!(matches!(
                next_action(&Outcome::ConnectFailed, &mut ever, &mut fails),
                Action::Reconnect(Some(_))
            ));
        }
        // ...then exits cleanly rather than pinning the process.
        assert_eq!(
            next_action(&Outcome::ConnectFailed, &mut ever, &mut fails),
            Action::Exit(0)
        );
    }
}
