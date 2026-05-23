//! `chevron event` — shell-side client for the command-lifecycle wire
//! protocol (chevron-1yn Phase 1).
//!
//! Three subcommands matching the shell hook contract:
//!
//! - `new-session`: print a fresh ULID for the shell to capture into
//!   its session-id env var. No daemon contact.
//! - `cmd-start <session-id> <cwd> <cmd>`: mint a ULID for the command,
//!   print it for the shell to stash in a preexec-set variable, then
//!   publish a `CMD_START` event to the daemon.
//! - `cmd-end <id> <exit> <duration-ms>`: publish a `CMD_END` event.
//!
//! All three exit `0` on success and on daemon-not-running. A lost
//! event becomes a missing history row; we deliberately don't surface
//! that to the shell, which would only confuse the user. The
//! `CHEVRON_HISTORY=0` opt-out is enforced shell-side (we never get
//! invoked), so this binary doesn't re-check it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::daemon::{client, proto};

/// Dispatch a `chevron event …` invocation. Returns the desired
/// process exit code.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("new-session") => {
            println!("{}", new_ulid());
            0
        }
        Some("cmd-start") => {
            // Args after the subcommand: session-id, cwd, cmd...
            // We join trailing args with spaces so callers can pass an
            // un-quoted command and we still get the full text.
            let Some(session_id) = args.get(1).cloned() else {
                eprintln!("Usage: chevron event cmd-start <session-id> <cwd> <cmd...>");
                return 1;
            };
            let Some(cwd) = args.get(2).map(PathBuf::from) else {
                eprintln!("Usage: chevron event cmd-start <session-id> <cwd> <cmd...>");
                return 1;
            };
            let cmd = if args.len() >= 4 {
                args[3..].join(" ")
            } else {
                // Empty cmd is allowed — Enter on an empty prompt. Don't
                // emit a CMD_START in that case; the shell hook should
                // also skip, but we double-check here.
                return 0;
            };
            let id = new_ulid();
            println!("{id}");
            let req = proto::Request::CmdStart(proto::CmdStartEvent {
                id,
                session_id,
                hostname: hostname(),
                cwd,
                cmd,
                started_at_ms: now_ms(),
            });
            // Best-effort: swallow the boolean — the shell already got
            // the id via stdout and the user shouldn't see an error if
            // the daemon happens to be down.
            let _ = client::try_publish_event(&req);
            0
        }
        Some("cmd-end") => {
            let Some(id) = args.get(1).cloned() else {
                eprintln!("Usage: chevron event cmd-end <id> <exit> <duration-ms>");
                return 1;
            };
            let exit_status: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
            let duration_ms: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let req = proto::Request::CmdEnd(proto::CmdEndEvent {
                id,
                finished_at_ms: now_ms(),
                duration_ms,
                exit_status,
                // Regular cmd-end (no chcap wrapping) leaves the
                // Phase-4 output fields unset on the wire.
                output_bytes: None,
                output_truncated: None,
            });
            let _ = client::try_publish_event(&req);
            0
        }
        _ => {
            eprintln!("Usage: chevron event <new-session|cmd-start|cmd-end>");
            1
        }
    }
}

fn new_ulid() -> String {
    ulid::Ulid::new().to_string()
}

fn now_ms() -> i64 {
    // SystemTime is wall-clock; we deliberately use it (rather than
    // Instant) so the stored timestamps are interpretable by `chevron
    // history`. Saturating-cast keeps us sane past year 2262.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn hostname() -> String {
    // `gethostname(3)` writes up to `buf.len()` bytes and null-terminates
    // unless the name is exactly that length. 256 covers RFC 1035's
    // 253-byte limit; an "unknown" fallback keeps the field non-empty.
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf and
    // sets errno on failure; passing a valid &mut [u8] of that length
    // is the documented contract.
    let rc = unsafe {
        libc::gethostname(
            buf.as_mut_ptr().cast::<libc::c_char>(),
            buf.len() as libc::size_t,
        )
    };
    if rc != 0 {
        return "unknown".to_string();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).map_or_else(|_| "unknown".to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ulid_returns_26_char_crockford_base32() {
        // ULID canonical string form is 26 chars of Crockford base32.
        let id = new_ulid();
        assert_eq!(id.len(), 26);
        // No lowercase, no I/L/O/U (Crockford excluded set).
        assert!(
            id.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        );
    }

    #[test]
    fn new_ulid_returns_distinct_values() {
        let a = new_ulid();
        let b = new_ulid();
        assert_ne!(a, b);
    }

    #[test]
    fn hostname_returns_non_empty_string() {
        let h = hostname();
        assert!(!h.is_empty());
    }

    #[test]
    fn now_ms_is_recent_unix_time() {
        // Should be well past year 2020 (1.6e12 ms) and well before
        // year 3000 (3.2e13 ms).
        let t = now_ms();
        assert!(t > 1_600_000_000_000, "got {t}");
        assert!(t < 32_503_680_000_000, "got {t}");
    }

    #[test]
    fn run_new_session_succeeds() {
        let args = vec!["new-session".to_string()];
        assert_eq!(run(&args), 0);
    }

    #[test]
    fn run_cmd_start_with_no_args_exits_nonzero() {
        let args = vec!["cmd-start".to_string()];
        assert_eq!(run(&args), 1);
    }

    #[test]
    fn run_cmd_end_with_no_args_exits_nonzero() {
        let args = vec!["cmd-end".to_string()];
        assert_eq!(run(&args), 1);
    }

    #[test]
    fn run_unknown_subcommand_exits_nonzero() {
        let args = vec!["bogus".to_string()];
        assert_eq!(run(&args), 1);
    }

    #[test]
    fn run_cmd_start_with_empty_cmd_is_silent_noop() {
        // The shell hook should skip empty commands, but if it doesn't,
        // we exit 0 without contacting the daemon. The lack of a daemon
        // in this test environment would otherwise cause a hang on the
        // socket connect attempt.
        let args = vec![
            "cmd-start".to_string(),
            "sess".to_string(),
            "/tmp".to_string(),
        ];
        assert_eq!(run(&args), 0);
    }
}
