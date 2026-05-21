//! State actor — owns the per-workdir `RepoStatus` cache plus an
//! filesystem watcher over each repo's gitdir.
//!
//! Single thread, message-driven via `std::sync::mpsc`. The state thread
//! never does libgit2 work; it stores/retrieves cache entries and drops
//! them in response to FS events. Handler threads compute on cache miss
//! and feed results back via [`StateMsg::Insert`].
//!
//! ## Phase 2: FS-watch invalidation
//!
//! On `Insert`, the state thread registers a recursive `notify` watch on
//! the repo's gitdir (the path returned by `repo.path()` — handles
//! submodules and worktrees, where `.git` is a file pointing elsewhere).
//! When any path under that gitdir changes, the watcher callback sends
//! [`StateMsg::FsEvent`] with the changed paths. The state thread walks
//! each path's ancestors, finds the registered gitdir, and drops the
//! cache entry. The next query recomputes from a fresh libgit2 walk.
//!
//! Working-tree edits (vim saves, build artifacts) don't touch the
//! gitdir, so they aren't caught by the watcher. A short TTL ([`TTL`],
//! 100 ms) bounds their staleness window — short enough that no human
//! notices the lag between save and prompt redraw.
//!
//! ## Bounded watch set
//!
//! Inotify watches are a finite kernel resource (8192 per user by
//! default). To stay well under any limit, [`MAX_WATCHES`] caps the
//! watched set; LRU eviction reclaims slots when a new repo wants in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::segments::git::RepoStatus;

/// Cache freshness window. Tight enough that working-tree edits not
/// covered by the gitdir watcher (vim saves, etc.) reach the prompt
/// within one frame.
pub const TTL: Duration = Duration::from_millis(100);

/// Soft cap on simultaneously-watched repos. Linux default
/// `fs.inotify.max_user_watches` is 8192 system-wide; a recursive watch
/// on `.git/` consumes one descriptor per subdirectory inside it (~5–20
/// per repo). 64 watched repos × 20 descriptors = 1280, safely under
/// the limit and well over the realistic number of repos any human
/// works on simultaneously.
pub const MAX_WATCHES: usize = 64;

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
    /// Store a freshly-computed status and (idempotently) register a
    /// filesystem watch on the repo's gitdir.
    Insert {
        workdir: PathBuf,
        git_dir: PathBuf,
        status: RepoStatus,
        computed_at: Instant,
    },
    /// Path(s) changed inside a watched gitdir. The state thread walks
    /// each path's ancestors to find the registered gitdir, then drops
    /// that workdir's cache entry. Sent by the notify watcher callback;
    /// also useful for hand-driven tests.
    FsEvent(Vec<PathBuf>),
    /// Stop the actor. Mainly for tests; production exits via signal.
    Shutdown,
}

/// Internal state owned by the actor loop.
struct State {
    cache: HashMap<PathBuf, CacheEntry>,
    /// `git_dir → workdir`. Keyed by gitdir so FS events (which arrive
    /// with paths inside the gitdir) can resolve back to the workdir.
    watches: HashMap<PathBuf, PathBuf>,
    /// `workdir → last query time`. Drives LRU eviction.
    last_query: HashMap<PathBuf, Instant>,
    watcher: Option<RecommendedWatcher>,
}

impl State {
    fn new(watcher: Option<RecommendedWatcher>) -> Self {
        Self {
            cache: HashMap::new(),
            watches: HashMap::new(),
            last_query: HashMap::new(),
            watcher,
        }
    }

    fn get(&mut self, workdir: &Path, ttl: Duration) -> Option<RepoStatus> {
        let now = Instant::now();
        self.last_query.insert(workdir.to_path_buf(), now);
        self.cache
            .get(workdir)
            .filter(|e| e.is_fresh(now, ttl))
            .map(|e| e.status.clone())
    }

    fn insert(&mut self, workdir: &Path, git_dir: &Path, status: RepoStatus, computed_at: Instant) {
        self.last_query
            .insert(workdir.to_path_buf(), Instant::now());
        self.cache.insert(
            workdir.to_path_buf(),
            CacheEntry {
                status,
                computed_at,
            },
        );
        if !self.watches.contains_key(git_dir) {
            if self.watches.len() >= MAX_WATCHES {
                self.evict_lru();
            }
            self.register_watch(git_dir, workdir);
        }
    }

    fn invalidate_for_event_paths(&mut self, paths: &[PathBuf]) {
        for p in paths {
            if let Some(workdir) = resolve_workdir(&self.watches, p) {
                let workdir = workdir.clone();
                self.cache.remove(&workdir);
            }
        }
    }

    fn register_watch(&mut self, git_dir: &Path, workdir: &Path) {
        // Track the logical watch unconditionally so LRU bookkeeping works
        // even if the real notify watcher is unavailable (e.g. when
        // construction failed at startup, or in tests that disable it).
        // A subsequent `Insert` for the same gitdir is a no-op via the
        // outer `contains_key` guard.
        self.watches
            .insert(git_dir.to_path_buf(), workdir.to_path_buf());
        if let Some(w) = self.watcher.as_mut() {
            // Gitdir may be a file (submodule / worktree) or missing
            // entirely; failure here is non-fatal because the cache
            // still works via TTL.
            let _ = w.watch(git_dir, RecursiveMode::Recursive);
        }
    }

    fn evict_lru(&mut self) {
        let Some(stale_workdir) = self
            .last_query
            .iter()
            .min_by_key(|(_, t)| **t)
            .map(|(k, _)| k.clone())
        else {
            return;
        };
        // Find the gitdir entry pointing at this workdir.
        let stale_git_dir = self
            .watches
            .iter()
            .find(|(_, w)| **w == stale_workdir)
            .map(|(g, _)| g.clone());
        if let (Some(git_dir), Some(w)) = (stale_git_dir.as_ref(), self.watcher.as_mut()) {
            let _ = w.unwatch(git_dir);
        }
        if let Some(g) = stale_git_dir {
            self.watches.remove(&g);
        }
        self.cache.remove(&stale_workdir);
        self.last_query.remove(&stale_workdir);
    }
}

/// Walk `path`'s ancestors looking for a gitdir registered in `watches`.
/// Returns the corresponding workdir if found. O(depth) — typical
/// gitdir paths are 3–5 components, so this is a handful of hash lookups.
fn resolve_workdir<'a>(watches: &'a HashMap<PathBuf, PathBuf>, path: &Path) -> Option<&'a PathBuf> {
    let mut ancestor = Some(path);
    while let Some(p) = ancestor {
        if let Some(workdir) = watches.get(p) {
            return Some(workdir);
        }
        ancestor = p.parent();
    }
    None
}

/// Drive the actor loop. Owns the cache and watcher for the lifetime of
/// the call.
#[allow(clippy::needless_pass_by_value)]
pub fn run(rx: Receiver<StateMsg>, tx: Sender<StateMsg>, ttl: Duration) {
    let watcher = make_watcher(&tx);
    run_inner(rx, watcher, ttl);
}

#[allow(clippy::needless_pass_by_value)]
fn run_inner(rx: Receiver<StateMsg>, watcher: Option<RecommendedWatcher>, ttl: Duration) {
    let mut state = State::new(watcher);

    while let Ok(msg) = rx.recv() {
        match msg {
            StateMsg::Get { workdir, reply } => {
                let hit = state.get(&workdir, ttl);
                let _ = reply.send(hit);
            }
            StateMsg::Insert {
                workdir,
                git_dir,
                status,
                computed_at,
            } => {
                state.insert(&workdir, &git_dir, status, computed_at);
            }
            StateMsg::FsEvent(paths) => {
                state.invalidate_for_event_paths(&paths);
            }
            StateMsg::Shutdown => break,
        }
    }
}

/// Build a `notify` watcher whose events feed back into the state
/// thread's own channel as [`StateMsg::FsEvent`]. Returns `None` if
/// watcher construction fails (e.g. inotify resource exhaustion); the
/// daemon stays functional via TTL-only invalidation in that case.
fn make_watcher(tx: &Sender<StateMsg>) -> Option<RecommendedWatcher> {
    let tx = tx.clone();
    notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            // Send error means state thread has shut down; nothing to do.
            let _ = tx.send(StateMsg::FsEvent(event.paths));
        }
    })
    .ok()
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
    let tx_for_run = tx.clone();
    let handle = std::thread::Builder::new()
        .name("chevrond-state".into())
        .spawn(move || run(rx, tx_for_run, ttl))?;
    Ok((tx, handle))
}

/// Test-only: spawn the actor without a real `notify` watcher. Logical
/// watch tracking still happens (so LRU and `FsEvent` invalidation can
/// be exercised) but no actual `notify` resources are created.
#[cfg(test)]
fn spawn_no_watcher(ttl: Duration) -> (Sender<StateMsg>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("chevrond-state-test".into())
        .spawn(move || run_inner(rx, None, ttl))
        .unwrap();
    (tx, handle)
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

    /// Test-only TTL. Production [`TTL`] is 100 ms, but unit tests incur
    /// message round-trips and (on macOS) `FSEventStream` setup that can
    /// approach that bound — we'd see false negatives on slow runners.
    /// Tests that specifically exercise TTL semantics override this.
    fn test_ttl() -> Duration {
        Duration::from_mins(1)
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

    fn insert(tx: &Sender<StateMsg>, workdir: &Path, git_dir: &Path, branch: &str) {
        tx.send(StateMsg::Insert {
            workdir: workdir.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
            status: fixture(branch),
            computed_at: Instant::now(),
        })
        .unwrap();
    }

    #[test]
    fn get_miss_returns_none() {
        let (tx, handle) = spawn(test_ttl()).unwrap();
        assert!(query(&tx, PathBuf::from("/nonexistent")).is_none());
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn insert_then_get_returns_fresh() {
        let (tx, handle) = spawn(test_ttl()).unwrap();
        let workdir = PathBuf::from("/repo");
        let git_dir = workdir.join(".git");
        insert(&tx, &workdir, &git_dir, "master");
        let got = query(&tx, workdir).expect("expected fresh hit");
        assert_eq!(got.branch, "master");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn stale_entry_returns_none() {
        // TTL = 50 ms, entry timestamped 500 ms ago → expired.
        let (tx, handle) = spawn(Duration::from_millis(50)).unwrap();
        let workdir = PathBuf::from("/repo");
        tx.send(StateMsg::Insert {
            workdir: workdir.clone(),
            git_dir: workdir.join(".git"),
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
        let (tx, handle) = spawn(test_ttl()).unwrap();
        let workdir = PathBuf::from("/repo");
        let git_dir = workdir.join(".git");
        insert(&tx, &workdir, &git_dir, "old");
        insert(&tx, &workdir, &git_dir, "new");
        let got = query(&tx, workdir).unwrap();
        assert_eq!(got.branch, "new");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn multiple_workdirs_independent() {
        let (tx, handle) = spawn(test_ttl()).unwrap();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        insert(&tx, &a, &a.join(".git"), "branch-a");
        insert(&tx, &b, &b.join(".git"), "branch-b");
        assert_eq!(query(&tx, a).unwrap().branch, "branch-a");
        assert_eq!(query(&tx, b).unwrap().branch, "branch-b");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn shutdown_drains_pending_messages() {
        // Messages queued before Shutdown should still be processed in
        // FIFO order. We rely on this for deterministic test cleanup.
        let (tx, handle) = spawn(test_ttl()).unwrap();
        let workdir = PathBuf::from("/repo");
        let git_dir = workdir.join(".git");
        insert(&tx, &workdir, &git_dir, "master");
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
        let (tx, handle) = spawn(test_ttl()).unwrap();
        let (reply_tx, reply_rx) = mpsc::channel();
        drop(reply_rx);
        tx.send(StateMsg::Get {
            workdir: PathBuf::from("/x"),
            reply: reply_tx,
        })
        .unwrap();
        let workdir = PathBuf::from("/y");
        let git_dir = workdir.join(".git");
        insert(&tx, &workdir, &git_dir, "alive");
        assert_eq!(query(&tx, workdir).unwrap().branch, "alive");
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    // ── Phase 2: FS-event invalidation ──────────────────────────────────

    #[test]
    fn fs_event_invalidates_matching_workdir() {
        // Drive an Insert via a real .git tempdir so the state thread can
        // actually register a watch; then synthesise an FsEvent for a
        // child of that gitdir and verify the cache entry is dropped.
        let tmp = tempfile::TempDir::new().unwrap();
        let workdir = tmp.path().to_path_buf();
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let (tx, handle) = spawn(test_ttl()).unwrap();
        insert(&tx, &workdir, &git_dir, "master");
        assert!(query(&tx, workdir.clone()).is_some());

        // Simulate the watcher seeing a change to .git/HEAD.
        tx.send(StateMsg::FsEvent(vec![git_dir.join("HEAD")]))
            .unwrap();
        assert!(
            query(&tx, workdir).is_none(),
            "cache should be dropped after FS event on watched gitdir"
        );

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fs_event_for_unrelated_path_is_ignored() {
        // FS event for a path nowhere near a registered gitdir must not
        // disturb existing cache entries.
        let tmp = tempfile::TempDir::new().unwrap();
        let workdir = tmp.path().to_path_buf();
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let (tx, handle) = spawn(test_ttl()).unwrap();
        insert(&tx, &workdir, &git_dir, "master");

        tx.send(StateMsg::FsEvent(vec![PathBuf::from("/var/log/something")]))
            .unwrap();
        assert!(query(&tx, workdir).is_some());

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn resolve_workdir_walks_ancestors() {
        let mut watches = HashMap::new();
        watches.insert(PathBuf::from("/r/.git"), PathBuf::from("/r"));

        // Direct match on the gitdir itself.
        assert_eq!(
            resolve_workdir(&watches, Path::new("/r/.git")),
            Some(&PathBuf::from("/r"))
        );
        // Path inside the gitdir.
        assert_eq!(
            resolve_workdir(&watches, Path::new("/r/.git/HEAD")),
            Some(&PathBuf::from("/r"))
        );
        // Deeper path.
        assert_eq!(
            resolve_workdir(&watches, Path::new("/r/.git/refs/heads/master")),
            Some(&PathBuf::from("/r"))
        );
        // Path outside the gitdir.
        assert_eq!(
            resolve_workdir(&watches, Path::new("/other/.git/HEAD")),
            None
        );
        // Workdir path (without `.git`) — also outside the gitdir.
        assert_eq!(resolve_workdir(&watches, Path::new("/r/src/main.rs")), None);
    }

    #[test]
    fn lru_eviction_at_cap() {
        // Use `spawn_no_watcher` to avoid creating 65 real `notify`
        // watches — that hits FSEventStream init latency on macOS and
        // makes the test multi-second. Logical watch tracking still
        // happens via the unconditional `watches` insert, so LRU is
        // exercised. Use throwaway PathBufs (no real dirs needed).
        let (tx, handle) = spawn_no_watcher(test_ttl());
        let workdirs: Vec<PathBuf> = (0..=MAX_WATCHES)
            .map(|i| PathBuf::from(format!("/tmp/lru-test-{i}")))
            .collect();
        for (i, w) in workdirs.iter().enumerate() {
            insert(&tx, w, &w.join(".git"), &format!("b{i}"));
            // Tiny spacing so last_query timestamps are strictly ordered.
            std::thread::sleep(Duration::from_millis(1));
        }
        // The first one inserted has the oldest last_query → evicted.
        assert!(query(&tx, workdirs[0].clone()).is_none());
        // The most recent one is still present.
        assert!(query(&tx, workdirs[MAX_WATCHES].clone()).is_some());

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }
}
