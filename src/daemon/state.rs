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
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;

use super::proto::{CmdEndEvent, CmdStartEvent, EventPayload};
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

/// Per-subscriber bounded mailbox depth. Slow subscribers (their
/// relay thread blocked on socket write because the peer's read end
/// is wedged) shed events past this cap rather than growing daemon
/// memory unboundedly. 32 is generous for the prompt-refresh use
/// case — bursts of FS events from `git rebase` etc. coalesce in
/// the broadcaster's mind anyway, and a single redraw catches up.
pub const SUBSCRIBER_CHANNEL_CAP: usize = 32;

/// Per-subscriber registration. `sender` is the actor's end of a
/// bounded channel; the listener-side relay thread owns the receiver
/// and writes each message to the socket. `cwd_filter` narrows
/// broadcasts; `None` means "all events". The id is opaque, just for
/// debugging logs and the actor's bookkeeping.
pub struct Subscriber {
    pub sender: SyncSender<EventPayload>,
    pub cwd_filter: Option<PathBuf>,
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
    /// Persist a command-started event. Fire-and-forget: the listener
    /// ACKs the client as soon as the message lands on this channel.
    /// Phase 1 of chevron-1yn; consumed by the SQLite-backed commands
    /// table.
    CmdStart(CmdStartEvent),
    /// Persist a command-finished event by updating the existing row
    /// keyed by `id`. Silent no-op if no matching row exists (e.g. the
    /// daemon was restarted between start and end).
    CmdEnd(CmdEndEvent),
    /// Register a Phase 3 subscriber. The actor adds it to the
    /// broadcast list and replies with its assigned id on `reply` so
    /// the listener can ACK back to the client only after the
    /// registration is durable (no events lost between ACK and
    /// register).
    Subscribe {
        subscriber: Subscriber,
        reply: Sender<u64>,
    },
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
    /// `SQLite` handle for the commands log (chevron-1yn Phase 1).
    /// Owned by the state thread so writes serialise naturally and we
    /// don't need a Mutex. Single-writer `SQLite` has no contention;
    /// queries against this file (Phase 2's `chevron history`) will
    /// open separate read-only connections under WAL.
    db: Connection,
    /// Phase 3 (chevron-1yn.3): live-prompt subscribers. Each entry
    /// is one connected `chevron subscribe` helper; the actor pushes
    /// `EventPayload`s into the channel when state changes, and the
    /// per-connection relay thread on the listener side drains them
    /// to its socket. Disconnected channels are pruned lazily on the
    /// next broadcast attempt.
    subscribers: HashMap<u64, Subscriber>,
    next_subscriber_id: u64,
}

impl State {
    fn new(watcher: Option<RecommendedWatcher>, db: Connection) -> Self {
        Self {
            cache: HashMap::new(),
            watches: HashMap::new(),
            last_query: HashMap::new(),
            watcher,
            db,
            subscribers: HashMap::new(),
            next_subscriber_id: 1,
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
        // Coalesce: a single git operation often fires many FS events
        // (HEAD, refs, index, ORIG_HEAD, …). Collect the distinct
        // workdirs affected and broadcast once per workdir instead of
        // once per path. The cache invalidation itself is per-path
        // (and idempotent) but the subscriber notification benefits
        // from deduping.
        let mut affected: Vec<PathBuf> = Vec::new();
        for p in paths {
            if let Some(workdir) = resolve_workdir(&self.watches, p) {
                let workdir = workdir.clone();
                self.cache.remove(&workdir);
                if !affected.contains(&workdir) {
                    affected.push(workdir);
                }
            }
        }
        for workdir in affected {
            self.broadcast(&EventPayload {
                topic: "git".to_string(),
                cwd: Some(workdir),
                id: None,
            });
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

    fn record_cmd_start(&self, e: &CmdStartEvent) {
        // INSERT OR IGNORE so a duplicate id (shouldn't happen with
        // client-side ULIDs but worth guarding against) doesn't error
        // out the actor. Logging an error here would write to stderr
        // which the daemon redirects to /dev/null — silent on duplicate
        // is the only reasonable behaviour.
        let _ = self.db.execute(
            "INSERT OR IGNORE INTO commands \
             (id, session_id, hostname, cwd, cmd, started_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                e.id,
                e.session_id,
                e.hostname,
                e.cwd.to_string_lossy(),
                e.cmd,
                e.started_at_ms,
            ],
        );
    }

    fn record_cmd_end(&self, e: &CmdEndEvent) {
        // UPDATE — silent no-op if no matching id (daemon restarted
        // between start and end, or end arrived for an event we never
        // saw). Phase 2's history query just won't see this row's
        // completion fields; it's still in the table.
        let _ = self.db.execute(
            "UPDATE commands \
             SET finished_at = ?2, duration_ms = ?3, exit_status = ?4 \
             WHERE id = ?1",
            rusqlite::params![e.id, e.finished_at_ms, e.duration_ms, e.exit_status,],
        );
    }

    /// Look up the cwd of a recorded command. Used when broadcasting
    /// `cmd` events because `CmdEndEvent` doesn't carry cwd (it's
    /// small on purpose — completions are matched to starts by id).
    /// Returns `None` if the row isn't present, which happens if
    /// Phase 1's `CmdStart` never landed for this id.
    fn cmd_cwd(&self, id: &str) -> Option<PathBuf> {
        self.db
            .query_row("SELECT cwd FROM commands WHERE id = ?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .ok()
            .map(PathBuf::from)
    }

    fn add_subscriber(&mut self, subscriber: Subscriber) -> u64 {
        let id = self.next_subscriber_id;
        self.next_subscriber_id = self.next_subscriber_id.wrapping_add(1);
        self.subscribers.insert(id, subscriber);
        id
    }

    /// Broadcast `payload` to subscribers whose `cwd_filter` matches
    /// (or is None). Lazy-prunes subscribers whose receiver has been
    /// dropped (relay thread exited because the socket closed). Full
    /// mailboxes drop the event for that subscriber but keep them
    /// registered — slow consumers shed events rather than growing
    /// daemon memory.
    fn broadcast(&mut self, payload: &EventPayload) {
        let mut dead: Vec<u64> = Vec::new();
        for (id, sub) in &self.subscribers {
            if !subscriber_matches(sub, payload) {
                continue;
            }
            // Full mailbox: slow subscriber; drop this event. Their
            // next successful delivery still triggers a redraw which
            // pulls the current state from the cache. So `Ok(())`
            // and `Err(Full)` are intentionally identical-bodied
            // here — both mean "this subscriber stays registered".
            #[allow(clippy::match_same_arms)]
            match sub.sender.try_send(payload.clone()) {
                Ok(()) => {}
                Err(TrySendError::Disconnected(_)) => dead.push(*id),
                Err(TrySendError::Full(_)) => {}
            }
        }
        for id in dead {
            self.subscribers.remove(&id);
        }
    }
}

fn subscriber_matches(sub: &Subscriber, payload: &EventPayload) -> bool {
    match (&sub.cwd_filter, &payload.cwd) {
        (None, _) => true,
        (Some(filter), Some(cwd)) => filter == cwd,
        // Subscriber asked for a specific cwd but this event has no
        // cwd (rare — cmd events for ids we don't have rows for).
        (Some(_), None) => false,
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

/// Drive the actor loop. Owns the cache, watcher, and `SQLite` handle
/// for the lifetime of the call.
#[allow(clippy::needless_pass_by_value)]
pub fn run(rx: Receiver<StateMsg>, tx: Sender<StateMsg>, ttl: Duration, db: Connection) {
    let watcher = make_watcher(&tx);
    run_inner(rx, watcher, ttl, db);
}

#[allow(clippy::needless_pass_by_value)]
fn run_inner(
    rx: Receiver<StateMsg>,
    watcher: Option<RecommendedWatcher>,
    ttl: Duration,
    db: Connection,
) {
    let mut state = State::new(watcher, db);

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
            StateMsg::CmdStart(e) => state.record_cmd_start(&e),
            StateMsg::CmdEnd(e) => {
                // Persist first so the broadcast's cmd_cwd() lookup
                // sees the row. CmdEnd-without-prior-CmdStart already
                // no-ops in record_cmd_end; we still emit the event
                // with the id (cwd will be None) so subscribers can
                // act on the signal even if persistence races. The
                // shell-prompt subscriber only cares about the signal
                // itself anyway.
                state.record_cmd_end(&e);
                let cwd = state.cmd_cwd(&e.id);
                state.broadcast(&EventPayload {
                    topic: "cmd".to_string(),
                    cwd,
                    id: Some(e.id),
                });
            }
            StateMsg::Subscribe { subscriber, reply } => {
                let id = state.add_subscriber(subscriber);
                // Confirm registration *before* the listener ACKs the
                // client. If reply fails (peer gone), the subscriber
                // is harmlessly left in the map until the next
                // broadcast prunes it as Disconnected.
                let _ = reply.send(id);
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
/// join handle. The actor takes ownership of `db` for its lifetime.
///
/// # Errors
///
/// Returns the [`io::Error`](std::io::Error) from `thread::Builder::spawn`
/// only if the OS can't create a thread (resource exhaustion).
pub fn spawn(ttl: Duration, db: Connection) -> std::io::Result<(Sender<StateMsg>, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let tx_for_run = tx.clone();
    let handle = std::thread::Builder::new()
        .name("chevrond-state".into())
        .spawn(move || run(rx, tx_for_run, ttl, db))?;
    Ok((tx, handle))
}

/// Open the commands database at `dir/commands.db`, apply the schema,
/// and return a write-mode connection. WAL + `synchronous=NORMAL`
/// keeps per-event writes well under a millisecond on SSD while still
/// letting concurrent readers (Phase 2's `chevron history`) see
/// committed rows.
///
/// # Errors
///
/// Returns the underlying [`rusqlite::Error`] if the directory isn't
/// writable, the file is corrupt, or the schema-application fails.
pub fn open_db(dir: &Path) -> rusqlite::Result<Connection> {
    let path = dir.join("commands.db");
    let conn = Connection::open(&path)?;
    apply_schema(&conn)?;
    Ok(conn)
}

/// Test-only counterpart to [`open_db`] that opens an in-memory DB. The
/// resulting connection is only usable from the thread that holds it,
/// which matches the state actor's ownership model.
///
/// # Errors
///
/// Returns the underlying [`rusqlite::Error`] if the in-memory DB
/// can't be initialised — should be impossible in practice but the
/// signature matches [`open_db`] for symmetry.
#[cfg(test)]
pub fn open_memory_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_schema(&conn)?;
    Ok(conn)
}

fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    // WAL gives concurrent readers + one writer with no journal-file
    // contention; synchronous=NORMAL gives durability up to the last
    // checkpoint, which is fine for command history (a crash losing the
    // last few seconds of commands is acceptable; corruption is not).
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', '1');

        CREATE TABLE IF NOT EXISTS commands (
            id           TEXT PRIMARY KEY,
            session_id   TEXT NOT NULL,
            hostname     TEXT NOT NULL,
            cwd          TEXT NOT NULL,
            cmd          TEXT NOT NULL,
            started_at   INTEGER NOT NULL,
            finished_at  INTEGER,
            duration_ms  INTEGER,
            exit_status  INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_commands_cwd          ON commands(cwd);
        CREATE INDEX IF NOT EXISTS idx_commands_started_at   ON commands(started_at);
        CREATE INDEX IF NOT EXISTS idx_commands_exit_status  ON commands(exit_status);",
    )?;
    Ok(())
}

/// Test-only: spawn the actor without a real `notify` watcher. Logical
/// watch tracking still happens (so LRU and `FsEvent` invalidation can
/// be exercised) but no actual `notify` resources are created.
#[cfg(test)]
fn spawn_no_watcher(ttl: Duration, db: Connection) -> (Sender<StateMsg>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("chevrond-state-test".into())
        .spawn(move || run_inner(rx, None, ttl, db))
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
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        assert!(query(&tx, PathBuf::from("/nonexistent")).is_none());
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn insert_then_get_returns_fresh() {
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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
        let (tx, handle) = spawn(Duration::from_millis(50), open_memory_db().unwrap()).unwrap();
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
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
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

    // ── Phase 1 (chevron-1yn.1): command lifecycle persistence ───────────

    fn fixture_cmd_start(id: &str) -> CmdStartEvent {
        CmdStartEvent {
            id: id.to_string(),
            session_id: "sess-abc".to_string(),
            hostname: "matt-mbp".to_string(),
            cwd: PathBuf::from("/Users/mim/src/chevron"),
            cmd: "cargo test".to_string(),
            started_at_ms: 1_000,
        }
    }

    fn fixture_cmd_end(id: &str) -> CmdEndEvent {
        CmdEndEvent {
            id: id.to_string(),
            finished_at_ms: 2_500,
            duration_ms: 1_500,
            exit_status: 0,
        }
    }

    /// Drain by sending Shutdown and joining the thread, which releases
    /// the actor's owned connection so a fresh reader can open the file
    /// without write-lock contention.
    fn drain(tx: &Sender<StateMsg>, handle: JoinHandle<()>) {
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn cmd_start_inserts_row_with_pending_completion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = open_db(tmp.path()).unwrap();
        let (tx, handle) = spawn(test_ttl(), db).unwrap();
        tx.send(StateMsg::CmdStart(fixture_cmd_start("id-1")))
            .unwrap();
        drain(&tx, handle);

        let conn = Connection::open(tmp.path().join("commands.db")).unwrap();
        let (cmd, started_at, finished_at, exit_status): (String, i64, Option<i64>, Option<i64>) =
            conn.query_row(
                "SELECT cmd, started_at, finished_at, exit_status FROM commands WHERE id = ?1",
                ["id-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(cmd, "cargo test");
        assert_eq!(started_at, 1_000);
        // Completion columns must be NULL until CmdEnd lands.
        assert!(finished_at.is_none());
        assert!(exit_status.is_none());
    }

    #[test]
    fn cmd_end_fills_completion_columns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = open_db(tmp.path()).unwrap();
        let (tx, handle) = spawn(test_ttl(), db).unwrap();
        tx.send(StateMsg::CmdStart(fixture_cmd_start("id-2")))
            .unwrap();
        tx.send(StateMsg::CmdEnd(fixture_cmd_end("id-2"))).unwrap();
        drain(&tx, handle);

        let conn = Connection::open(tmp.path().join("commands.db")).unwrap();
        let (finished_at, duration_ms, exit_status): (i64, i64, i64) = conn
            .query_row(
                "SELECT finished_at, duration_ms, exit_status FROM commands WHERE id = ?1",
                ["id-2"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(finished_at, 2_500);
        assert_eq!(duration_ms, 1_500);
        assert_eq!(exit_status, 0);
    }

    #[test]
    fn cmd_end_without_start_is_silent_noop() {
        // A CmdEnd referencing an unknown id (daemon restarted between
        // start and end, or the start was opt-ed-out via leading space)
        // must not error the actor or insert a partial row.
        let tmp = tempfile::TempDir::new().unwrap();
        let db = open_db(tmp.path()).unwrap();
        let (tx, handle) = spawn(test_ttl(), db).unwrap();
        tx.send(StateMsg::CmdEnd(fixture_cmd_end("orphan")))
            .unwrap();
        // The actor should still be alive for follow-up traffic.
        tx.send(StateMsg::CmdStart(fixture_cmd_start("id-3")))
            .unwrap();
        drain(&tx, handle);

        let conn = Connection::open(tmp.path().join("commands.db")).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "only the successful CmdStart should have a row");
        let has_orphan: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE id = ?1",
                ["orphan"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_orphan, 0);
    }

    #[test]
    fn duplicate_cmd_start_ignores_second_insert() {
        // Client-side ULIDs shouldn't repeat, but if they do (e.g. test
        // fixture replay), the second insert must be a no-op rather
        // than blowing up the actor with a UNIQUE constraint error.
        let tmp = tempfile::TempDir::new().unwrap();
        let db = open_db(tmp.path()).unwrap();
        let (tx, handle) = spawn(test_ttl(), db).unwrap();
        tx.send(StateMsg::CmdStart(fixture_cmd_start("dup")))
            .unwrap();
        let mut second = fixture_cmd_start("dup");
        second.cmd = "second insert wins? (no)".to_string();
        tx.send(StateMsg::CmdStart(second)).unwrap();
        drain(&tx, handle);

        let conn = Connection::open(tmp.path().join("commands.db")).unwrap();
        let cmd: String = conn
            .query_row("SELECT cmd FROM commands WHERE id = ?1", ["dup"], |row| {
                row.get(0)
            })
            .unwrap();
        // INSERT OR IGNORE keeps the first one.
        assert_eq!(cmd, "cargo test");
    }

    #[test]
    fn open_db_seeds_schema_version_and_is_idempotent() {
        // Opening twice on the same dir must succeed (CREATE TABLE IF
        // NOT EXISTS + INSERT OR IGNORE keep things deterministic) and
        // the schema_version row must read back as '1'.
        let tmp = tempfile::TempDir::new().unwrap();
        let _ = open_db(tmp.path()).unwrap();
        let conn = open_db(tmp.path()).unwrap();
        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
    }

    // ── Phase 3 (chevron-1yn.3): subscriber broadcast ───────────────────

    fn subscribe(tx: &Sender<StateMsg>) -> (Receiver<EventPayload>, u64) {
        let (event_tx, event_rx) = mpsc::sync_channel(SUBSCRIBER_CHANNEL_CAP);
        subscribe_with(
            tx,
            Subscriber {
                sender: event_tx,
                cwd_filter: None,
            },
        )
        .map(|id| (event_rx, id))
        .unwrap()
    }

    fn subscribe_with(tx: &Sender<StateMsg>, subscriber: Subscriber) -> Result<u64, String> {
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.send(StateMsg::Subscribe {
            subscriber,
            reply: reply_tx,
        })
        .map_err(|e| e.to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn subscribe_returns_distinct_ids() {
        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        let (_rx1, id1) = subscribe(&tx);
        let (_rx2, id2) = subscribe(&tx);
        assert_ne!(id1, id2);
        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fs_event_broadcasts_to_subscribers() {
        // Drive a real .git tempdir + Insert so the workdir is in the
        // watches map (FsEvent only fires for known watches).
        let tmp = tempfile::TempDir::new().unwrap();
        let workdir = tmp.path().to_path_buf();
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        insert(&tx, &workdir, &git_dir, "master");
        let (event_rx, _id) = subscribe(&tx);

        tx.send(StateMsg::FsEvent(vec![git_dir.join("HEAD")]))
            .unwrap();

        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("expected a broadcast event");
        assert_eq!(event.topic, "git");
        assert_eq!(event.cwd, Some(workdir));

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn fs_event_coalesces_multiple_paths_per_workdir() {
        // A real git operation fires N events per logical change
        // (HEAD, refs, index, …). The broadcaster should emit ONE
        // event per affected workdir, not N.
        let tmp = tempfile::TempDir::new().unwrap();
        let workdir = tmp.path().to_path_buf();
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        insert(&tx, &workdir, &git_dir, "master");
        let (event_rx, _id) = subscribe(&tx);

        tx.send(StateMsg::FsEvent(vec![
            git_dir.join("HEAD"),
            git_dir.join("index"),
            git_dir.join("refs/heads/master"),
        ]))
        .unwrap();

        // Exactly one event.
        let first = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(first.topic, "git");
        let second = event_rx.recv_timeout(Duration::from_millis(100));
        assert!(second.is_err(), "expected coalescing but got: {second:?}");

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn subscriber_cwd_filter_skips_other_workdirs() {
        let tmp_a = tempfile::TempDir::new().unwrap();
        let tmp_b = tempfile::TempDir::new().unwrap();
        let workdir_a = tmp_a.path().to_path_buf();
        let workdir_b = tmp_b.path().to_path_buf();
        let git_a = workdir_a.join(".git");
        let git_b = workdir_b.join(".git");
        std::fs::create_dir_all(&git_a).unwrap();
        std::fs::create_dir_all(&git_b).unwrap();

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        insert(&tx, &workdir_a, &git_a, "master");
        insert(&tx, &workdir_b, &git_b, "master");

        let (event_tx, event_rx) = mpsc::sync_channel(SUBSCRIBER_CHANNEL_CAP);
        let _id = subscribe_with(
            &tx,
            Subscriber {
                sender: event_tx,
                cwd_filter: Some(workdir_a.clone()),
            },
        )
        .unwrap();

        // Event for B should be filtered out.
        tx.send(StateMsg::FsEvent(vec![git_b.join("HEAD")]))
            .unwrap();
        let no_event = event_rx.recv_timeout(Duration::from_millis(200));
        assert!(no_event.is_err(), "got unexpected: {no_event:?}");

        // Event for A should reach the subscriber.
        tx.send(StateMsg::FsEvent(vec![git_a.join("HEAD")]))
            .unwrap();
        let event = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.cwd, Some(workdir_a));

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn disconnected_subscriber_pruned_on_next_broadcast() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workdir = tmp.path().to_path_buf();
        let git_dir = workdir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();

        let (tx, handle) = spawn(test_ttl(), open_memory_db().unwrap()).unwrap();
        insert(&tx, &workdir, &git_dir, "master");

        let (event_tx, event_rx) = mpsc::sync_channel(SUBSCRIBER_CHANNEL_CAP);
        let _id = subscribe_with(
            &tx,
            Subscriber {
                sender: event_tx,
                cwd_filter: None,
            },
        )
        .unwrap();
        // Drop the receiver — actor's next try_send fails with Disconnected.
        drop(event_rx);

        // First broadcast surfaces Disconnected and prunes. Second
        // would no longer reach this subscriber. We can't easily
        // observe pruning from outside the actor, but we can verify
        // a fresh subscription works after the dead one churned.
        tx.send(StateMsg::FsEvent(vec![git_dir.join("HEAD")]))
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let (event_rx2, _) = subscribe(&tx);
        tx.send(StateMsg::FsEvent(vec![git_dir.join("HEAD")]))
            .unwrap();
        let event = event_rx2.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.topic, "git");

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn cmd_end_broadcasts_cmd_event_with_cwd() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = open_db(tmp.path()).unwrap();
        let (tx, handle) = spawn(test_ttl(), db).unwrap();

        let (event_rx, _) = subscribe(&tx);

        // Start then end a command; verify a cmd event lands with the
        // cwd we recorded at start.
        tx.send(StateMsg::CmdStart(fixture_cmd_start("cmd-evt-1")))
            .unwrap();
        tx.send(StateMsg::CmdEnd(fixture_cmd_end("cmd-evt-1")))
            .unwrap();

        // Drain: the CmdStart triggers no broadcast; the CmdEnd does.
        let event = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.topic, "cmd");
        assert_eq!(event.id.as_deref(), Some("cmd-evt-1"));
        assert!(event.cwd.is_some());

        tx.send(StateMsg::Shutdown).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn lru_eviction_at_cap() {
        // Use `spawn_no_watcher` to avoid creating 65 real `notify`
        // watches — that hits FSEventStream init latency on macOS and
        // makes the test multi-second. Logical watch tracking still
        // happens via the unconditional `watches` insert, so LRU is
        // exercised. Use throwaway PathBufs (no real dirs needed).
        let (tx, handle) = spawn_no_watcher(test_ttl(), open_memory_db().unwrap());
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
