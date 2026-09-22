//! Client-side event spool — the durability net under the lossy publish.
//!
//! `chevron event` publishes lifecycle events with a deliberately tiny
//! socket budget (see `client`'s `PUBLISH_TIMEOUT`): a shell hook must
//! never stall on a slow daemon, so under load events are DROPPED rather
//! than delivered late. That is the right trade for prompt latency and
//! the wrong one for history — the commands table silently grew holes
//! whenever the machine was busy or the daemon wasn't up yet.
//!
//! The spool closes the gap. A failed publish writes the encoded request
//! to `$socket_dir/spool/` as its own file (tmp + rename, so concurrent
//! shells never interleave and the daemon never reads a half-written
//! entry); the daemon drains the directory at startup and on every
//! watchdog tick. Replay is idempotent: `CMD_START` inserts with
//! OR IGNORE and `CMD_END` is a keyed update, so an event that was both
//! delivered and spooled (the ack was lost in flight) lands once.
//!
//! File names are `<nanos>-<event-id>-<kind>-<pid>.req`, built so plain
//! lexical order is replay order: the zero-padded nanosecond timestamp
//! leads, and the kind words are chosen so `begin` sorts before
//! `finish` — an instant command whose start and end land on the same
//! timestamp still replays in pair order (a ULID prefix flunked this:
//! within one millisecond its tail is random, and an end replayed
//! before its start no-ops against the missing row forever). The event
//! id lets `cmd-end` detect that its `cmd-start` is still queued and
//! follow it into the spool rather than racing ahead over the wire;
//! the pid is a pure uniquifier across concurrent shells.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use super::state::StateMsg;
use super::{paths, proto};

/// Upper bound on queued spool entries. With no daemon ever running
/// (or one that can't start), the spool would otherwise grow one file
/// per command forever; past the cap new events degrade to the old
/// drop-silently behaviour. 1024 commands is hours of typing — far
/// more than any realistic daemon outage.
const SPOOL_CAP: usize = 1024;

/// Extension for complete, replayable entries. In-flight writes use
/// `.tmp` and are ignored by the reader until renamed.
const ENTRY_EXT: &str = "req";

/// Write `req` to the spool. Returns `true` if the entry was durably
/// queued. `false` means the event is lost, exactly as before the spool
/// existed: the spool is over cap, the request isn't a lifecycle event,
/// or the filesystem said no.
pub fn spool_event(req: &proto::Request) -> bool {
    let (event_id, kind) = match req {
        proto::Request::CmdStart(e) => (&e.id, "begin"),
        proto::Request::CmdEnd(e) => (&e.id, "finish"),
        _ => return false,
    };
    let dir = paths::spool_dir();
    if fs::create_dir_all(&dir).is_err() {
        return false;
    }
    // Owner-only, like the socket dir: entries carry command lines. A
    // chmod failure means the dir isn't ours (e.g. a squatter
    // pre-created the /tmp fallback socket dir) — drop the event
    // rather than write command lines into a directory we don't
    // control.
    let Ok(meta) = fs::metadata(&dir) else {
        return false;
    };
    let mut perms = meta.permissions();
    perms.set_mode(0o700);
    if fs::set_permissions(&dir, perms).is_err() {
        return false;
    }
    if entries(&dir).len() >= SPOOL_CAP {
        return false;
    }
    // Defensive: the event id lands in a file name. ULIDs are plain
    // Crockford base32, but a hand-crafted id must not traverse paths.
    let safe_id: String = event_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = format!(
        "{nanos:020}-{safe_id}-{kind}-{}.{ENTRY_EXT}",
        std::process::id()
    );
    let tmp = dir.join(format!("{name}.tmp"));
    let mut line = proto::encode_request(req);
    line.push('\n');
    if fs::write(&tmp, line).is_err() {
        return false;
    }
    fs::rename(&tmp, dir.join(name)).is_ok()
}

/// Whether a `CMD_START` for `event_id` is still queued. `cmd-end`
/// consults this before publishing: an end delivered live while its
/// start sits in the spool would no-op against the missing row, and
/// the pair would never complete once the start replays.
#[must_use]
pub fn has_spooled_start(event_id: &str) -> bool {
    let needle = format!("-{event_id}-begin-");
    entries(&paths::spool_dir()).iter().any(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().contains(&needle))
    })
}

/// Drain the spool into the state actor, oldest first. Called by the
/// daemon at startup and on every watchdog tick. Returns the number of
/// events dispatched.
///
/// Entries are unlinked only after their message is queued; a crash in
/// between replays the entry next time, which the idempotent insert and
/// keyed update absorb. Entries that no longer decode to a lifecycle
/// event are unlinked without dispatch — a poisoned entry must not
/// wedge the drain forever.
#[must_use]
pub fn ingest(state_tx: &Sender<StateMsg>) -> usize {
    let mut sent = 0;
    for path in entries(&paths::spool_dir()) {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let msg = match proto::decode_request(content.lines().next().unwrap_or("")) {
            Ok(proto::Request::CmdStart(e)) => Some(StateMsg::CmdStart(e)),
            Ok(proto::Request::CmdEnd(e)) => Some(StateMsg::CmdEnd(e)),
            _ => None,
        };
        if let Some(msg) = msg {
            if state_tx.send(msg).is_err() {
                // Actor gone (shutdown mid-drain). Keep the entry for
                // the next daemon.
                return sent;
            }
            sent += 1;
        }
        let _ = fs::remove_file(&path);
    }
    sent
}

/// Complete spool entries, sorted so lexical (= chronological, thanks
/// to the leading zero-padded timestamp) order drives replay.
fn entries(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == ENTRY_EXT))
        .collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state;
    use serial_test::serial;
    use std::path::Path;
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn with_spool_dir(dir: &Path, f: impl FnOnce()) {
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", dir) };
        f();
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    fn start_event(id: &str) -> proto::Request {
        proto::Request::CmdStart(proto::CmdStartEvent {
            id: id.to_string(),
            session_id: "sess".into(),
            hostname: "host".into(),
            cwd: "/tmp".into(),
            cmd: "echo spooled".into(),
            started_at_ms: 1,
        })
    }

    fn end_event(id: &str) -> proto::Request {
        proto::Request::CmdEnd(proto::CmdEndEvent {
            id: id.to_string(),
            finished_at_ms: 2,
            duration_ms: 1,
            exit_status: 0,
            output_bytes: None,
            output_truncated: None,
        })
    }

    #[test]
    #[serial]
    fn spool_then_ingest_round_trips_through_the_actor() {
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            assert!(spool_event(&start_event("01ARZ3NDEKTSV4RRFFQ69G5FAA")));
            assert!(spool_event(&end_event("01ARZ3NDEKTSV4RRFFQ69G5FAA")));

            let db = state::open_memory_db().unwrap();
            let (tx, join) = state::spawn_no_watcher(state::TTL, db).unwrap();
            assert_eq!(ingest(&tx), 2);
            tx.send(StateMsg::Shutdown).unwrap();
            join.join().unwrap();

            assert!(
                entries(&paths::spool_dir()).is_empty(),
                "ingest must unlink drained entries"
            );
        });
    }

    #[test]
    #[serial]
    fn start_replays_before_its_end() {
        // Spooled back to back, the pair can land on the SAME
        // timestamp — the kind words are the tiebreaker (`begin` sorts
        // before `finish`). A ULID-prefixed name flaked exactly here:
        // its sub-millisecond tail is random.
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            for _ in 0..50 {
                assert!(spool_event(&start_event("01BX5ZZKBKACTAV9WEVGEMMVRZ")));
                assert!(spool_event(&end_event("01BX5ZZKBKACTAV9WEVGEMMVRZ")));
                let names = entries(&paths::spool_dir());
                assert_eq!(names.len(), 2);
                let first = names[0].file_name().unwrap().to_string_lossy();
                assert!(
                    first.contains("-begin-"),
                    "lexical order must put the start first, got {first}"
                );
                for p in names {
                    fs::remove_file(p).unwrap();
                }
            }
        });
    }

    #[test]
    #[serial]
    fn has_spooled_start_matches_only_its_id() {
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            assert!(!has_spooled_start("01BX5ZZKBKACTAV9WEVGEMMVRZ"));
            assert!(spool_event(&start_event("01BX5ZZKBKACTAV9WEVGEMMVRZ")));
            assert!(has_spooled_start("01BX5ZZKBKACTAV9WEVGEMMVRZ"));
            assert!(!has_spooled_start("01BX5ZZKBKACTAV9WEVGEMMVRA"));
        });
    }

    #[test]
    #[serial]
    fn spool_refuses_past_the_cap() {
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            let dir = paths::spool_dir();
            fs::create_dir_all(&dir).unwrap();
            for i in 0..SPOOL_CAP {
                fs::write(dir.join(format!("{i:08}-x-start.req")), "x\n").unwrap();
            }
            assert!(
                !spool_event(&start_event("01BX5ZZKBKACTAV9WEVGEMMVRZ")),
                "a full spool must degrade to the old drop behaviour"
            );
        });
    }

    #[test]
    #[serial]
    fn poisoned_entries_are_unlinked_not_replayed() {
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            let dir = paths::spool_dir();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("00000000-junk-start.req"), "NOT A REQUEST\n").unwrap();
            let (tx, _rx) = mpsc::channel();
            assert_eq!(ingest(&tx), 0);
            assert!(
                entries(&dir).is_empty(),
                "poison must be removed so it can't wedge the drain"
            );
        });
    }

    #[test]
    #[serial]
    fn tmp_files_are_invisible_to_reader_and_matcher() {
        let tmp = TempDir::new().unwrap();
        with_spool_dir(tmp.path(), || {
            let dir = paths::spool_dir();
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("00000000000000000000-id-begin-1.req.tmp"),
                "partial",
            )
            .unwrap();
            let (tx, _rx) = mpsc::channel();
            assert_eq!(ingest(&tx), 0);
            assert!(!has_spooled_start("id"));
        });
    }
}
