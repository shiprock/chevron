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

/// Dispatch a `chevron subscribe …` invocation. Exit codes:
///   - 0: clean disconnect (EOF, daemon shutdown, peer close)
///   - 1: argument error
///   - 2: connect or handshake failure (no daemon running, bad ack)
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

    let Ok(conn) = UnixStream::connect(paths::socket_path()) else {
        eprintln!("chevron subscribe: daemon not running");
        return 2;
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
        return 2;
    }
    if reader.read_line(&mut line).is_err() {
        return 2;
    }
    match proto::decode_response(&line) {
        Ok(proto::Response::Hello(v)) if v == proto::PROTO_VERSION => {}
        _ => {
            eprintln!("chevron subscribe: HELLO failed");
            return 2;
        }
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
        return 2;
    }
    if reader.read_line(&mut line).is_err() {
        return 2;
    }
    match proto::decode_response(&line) {
        Ok(proto::Response::Ack) => {}
        Ok(proto::Response::Err(reason)) => {
            eprintln!("chevron subscribe: daemon rejected: {reason}");
            return 2;
        }
        _ => {
            eprintln!("chevron subscribe: unexpected ack");
            return 2;
        }
    }

    // Now in subscriber-relay mode. The daemon's PING heartbeats can
    // arrive minutes apart, so we drop the per-read timeout — we want
    // to block on the socket indefinitely until something arrives.
    let _ = conn.set_read_timeout(None);

    let mut stdout = std::io::stdout().lock();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // EOF or socket error: daemon closed, shutting down, or
            // any I/O failure — treat all as clean exit so the shell
            // doesn't see a confusing failure code on rolling restart.
            Ok(0) | Err(_) => return 0,
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
                // Shell's read end of our stdout closed — exit cleanly.
                return 0;
            }
            if stdout.flush().is_err() {
                return 0;
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
    fn run_exits_2_when_no_daemon() {
        // Point at a definitely-missing socket dir to avoid any real
        // daemon on the dev box.
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", tmp.path()) };
        assert_eq!(run(&[]), 2);
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }
}
