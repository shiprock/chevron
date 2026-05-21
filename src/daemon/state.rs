//! State actor — owns the per-workdir `RepoStatus` cache.
//!
//! Single thread, message-driven via `std::sync::mpsc`. The state thread
//! never does I/O or libgit2 work; it just stores and retrieves entries with
//! a TTL check. Handler threads compute on cache miss and feed results back
//! via [`StateMsg::Insert`]. This separation keeps a slow compute on one
//! repo from blocking lookups on others.
//!
//! ## Lifetime
//!
//! [`run`] loops on `rx.recv()` until the sender drops (all handlers exited)
//! or [`StateMsg::Shutdown`] arrives. Production daemons exit via process
//! termination; `Shutdown` exists mainly so tests can join the thread
//! deterministically.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::segments::git::RepoStatus;

/// Phase-1 cache freshness window. Picked to cover normal interactive
/// redraw rhythms (typing, arrow-keys in history) without serving wildly
/// stale data when the working tree changes between prompts.
pub const TTL: Duration = Duration::from_secs(1);

struct CacheEntry {
    status: RepoStatus,
    computed_at: Instant,
}

impl CacheEntry {
    fn is_fresh(&self, now: Instant, ttl: Duration) -> bool {
        now.duration_since(self.computed_at) < ttl
    }
}

pub enum StateMsg {
    /// Look up a fresh entry. `reply` receives `Some(status)` only when the
    /// entry exists and is within `ttl`; `None` for miss or stale.
    Get {
        workdir: PathBuf,
        reply: Sender<Option<RepoStatus>>,
    },
    /// Store a freshly-computed status, overwriting any previous entry.
    Insert {
        workdir: PathBuf,
        status: RepoStatus,
        computed_at: Instant,
    },
    /// Stop the actor. Mainly for tests; production exits via signal.
    Shutdown,
}

/// Drive the actor loop. Owns the cache for the lifetime of the call.
///
/// Takes the receiver by value so the channel closes naturally when the
/// actor exits (drops `rx`). Passing by reference would let the caller keep
/// the channel alive past the actor — wrong invariant.
#[allow(clippy::needless_pass_by_value)]
pub fn run(rx: Receiver<StateMsg>, ttl: Duration) {
    let mut cache: HashMap<PathBuf, CacheEntry> = HashMap::new();
    while let Ok(msg) = rx.recv() {
        match msg {
            StateMsg::Get { workdir, reply } => {
                let now = Instant::now();
                let hit = cache
                    .get(&workdir)
                    .filter(|e| e.is_fresh(now, ttl))
                    .map(|e| e.status.clone());
                // Reply receiver may have been dropped if the handler thread
                // gave up (e.g. client disconnect). Nothing to do here —
                // the entry is unchanged either way.
                let _ = reply.send(hit);
            }
            StateMsg::Insert {
                workdir,
                status,
                computed_at,
            } => {
                cache.insert(
                    workdir,
                    CacheEntry {
                        status,
                        computed_at,
                    },
                );
            }
            StateMsg::Shutdown => break,
        }
    }
}

/// Spawn the state actor on a named thread. Returns the channel sender plus a
/// join handle.
///
/// # Errors
///
/// Returns the [`io::Error`](std::io::Error) from `thread::Builder::spawn`
/// only if the OS can't create a thread (resource exhaustion).
pub fn spawn(ttl: Duration) -> std::io::Result<(Sender<StateMsg>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("chevrond-state".into())
        .spawn(move || run(rx, ttl))?;
    Ok((tx, handle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segments::git::RepoStatus;

    fn fixture(branch: &str) -> RepoStatus {
        RepoStatus {
            repo_name: "chevron".to_string(),
            branch: branch.to_string(),
            detached: false,
            state: None,
            staged: 0,
            modified: 0,
            untracked: 0,
            conflicted: 0,
            ahead: 0,
            behind: 0,
            stashed: 0,
        }
    }

    fn make_past(elapsed: Duration) -> Instant {
        // Process is always old enough on test machines, but fall back if
        // checked_sub somehow returns None.
        Instant::now()
            .checked_sub(elapsed)
            .unwrap_or_else(Instant::now)
    }

    fn query(tx: &Sender<StateMsg>, workdir: PathBuf) -> Option<RepoStatus> {
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(StateMsg::Get {
            workdir,
            reply: reply_tx,
        })
        .unwrap();
        reply_rx.recv_timeout(Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn get_miss_returns_none() {
        let (tx, handle) = spawn(TTL).unwrap();
        assert!(query(&tx, PathBuf::from("/nonexistent")).is_none());
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn insert_then_get_returns_fresh() {
        let (tx, handle) = spawn(TTL).unwrap();
        let workdir = PathBuf::from("/repo");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("master"),
            computed_at: Instant::now(),
        })
        .unwrap();
        let got = query(&tx, workdir).expect("expected fresh hit");
        assert_eq!(got.branch, "master");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn stale_entry_returns_none() {
        // TTL = 100 ms, entry timestamped 500 ms ago → expired.
        let (tx, handle) = spawn(Duration::from_millis(100)).unwrap();
        let workdir = PathBuf::from("/repo");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("master"),
            computed_at: make_past(Duration::from_millis(500)),
        })
        .unwrap();
        assert!(query(&tx, workdir).is_none());
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn insert_overwrites_previous_entry() {
        let (tx, handle) = spawn(TTL).unwrap();
        let workdir = PathBuf::from("/repo");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("old"),
            computed_at: Instant::now(),
        })
        .unwrap();
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("new"),
            computed_at: Instant::now(),
        })
        .unwrap();
        let got = query(&tx, workdir).unwrap();
        assert_eq!(got.branch, "new");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn multiple_workdirs_independent() {
        let (tx, handle) = spawn(TTL).unwrap();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        tx.send(StateMsg::Insert {
            workdir: a.clone(),
            status: fixture("branch-a"),
            computed_at: Instant::now(),
        })
        .unwrap();
        tx.send(StateMsg::Insert {
            workdir: b.clone(),
            status: fixture("branch-b"),
            computed_at: Instant::now(),
        })
        .unwrap();
        assert_eq!(query(&tx, a).unwrap().branch, "branch-a");
        assert_eq!(query(&tx, b).unwrap().branch, "branch-b");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn shutdown_drains_pending_messages() {
        // Messages queued before Shutdown should still be processed in
        // FIFO order. We rely on this for deterministic test cleanup.
        let (tx, handle) = spawn(TTL).unwrap();
        let workdir = PathBuf::from("/repo");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("master"),
            computed_at: Instant::now(),
        })
        .unwrap();
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(StateMsg::Get {
            workdir,
            reply: reply_tx,
        })
        .unwrap();
        tx.send(StateMsg::Shutdown).unwrap();
        let got = reply_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(got.is_some());
        handle.join().unwrap();
    }

    #[test]
    fn dropped_reply_does_not_crash_actor() {
        // If the handler thread gives up before the reply lands, the state
        // thread's `reply.send(...)` returns Err — but the actor must keep
        // processing subsequent messages.
        let (tx, handle) = spawn(TTL).unwrap();
        let (reply_tx, reply_rx) = mpsc::channel();
        drop(reply_rx); // Receiver gone before we even ask.
        tx.send(StateMsg::Get {
            workdir: PathBuf::from("/x"),
            reply: reply_tx,
        })
        .unwrap();
        // Subsequent Insert + Get should still work.
        let workdir = PathBuf::from("/y");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            status: fixture("alive"),
            computed_at: Instant::now(),
        })
        .unwrap();
        assert_eq!(query(&tx, workdir).unwrap().branch, "alive");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }
}
