//! chevrond — TTL-cached daemon serving git `RepoStatus` over a Unix socket.
//!
//! ## Why
//!
//! Every prompt redraw recomputes git status from scratch. libgit2 status on a
//! medium repo costs 1–5 ms; the user notices when typing fast. A long-lived
//! daemon that keeps state hot per-repo and answers in ~50 µs eliminates that
//! perceptible delay.
//!
//! ## Architecture
//!
//! - **Listener thread** owns the `UnixListener`. Each accepted connection is
//!   handed to a freshly spawned handler thread (one thread per connection).
//! - **Handler thread** parses the request, runs `Repository::discover` to find
//!   the workdir, asks the state thread for a fresh entry, and on miss runs
//!   `RepoStatus::compute` itself before sending the result back via `Insert`.
//!   Keeping compute off the state thread means a single slow repo can't stall
//!   queries for other repos.
//! - **State thread** owns the `HashMap<PathBuf, CacheEntry>`. Two message
//!   kinds: `Get` (returns `Option<RepoStatus>` only if the entry is within
//!   the TTL) and `Insert`. No I/O, no libgit2.
//!
//! All inter-thread comms via `std::sync::mpsc`. No tokio, no async runtime —
//! prompt workloads are low-QPS and the actor model gives clean panic
//! isolation + trivial unit tests (drive the state thread by channel, no
//! sockets needed).
//!
//! ## Wire protocol
//!
//! See [`proto`] for the full spec. Line-oriented text over a Unix domain
//! socket, kv-encoded responses with percent-escaping for string fields. This
//! keeps the protocol debuggable with `socat - UNIX-CONNECT:…/chevrond.sock`.
//!
//! ## Phase 1 scope
//!
//! Only TTL-based invalidation (1 second). Filesystem watching arrives in
//! phase 2 (chevron-1yz.4), `core.fsmonitor` in phase 3 (chevron-1yz.2).

#[cfg(feature = "daemon")]
pub mod client;
#[cfg(feature = "daemon")]
pub mod lifecycle;
#[cfg(feature = "daemon")]
pub mod listener;
#[cfg(feature = "daemon")]
pub mod paths;
#[cfg(feature = "daemon")]
pub mod proto;
#[cfg(feature = "daemon")]
pub mod state;

use std::path::Path;

use crate::segments::git::RepoStatus;

/// Unified status entry point used by `chevron git`, the prompt path, and
/// `chevron tmux-title`. With the `daemon` feature enabled (default), this
/// tries the running daemon first and falls back to inline compute on any
/// failure or when the env var `CHEVRON_NO_DAEMON=1` is set. With the
/// feature disabled, it always computes inline.
///
/// `cwd` is canonicalized before being sent to the daemon so the cache key
/// is stable across symlinks and relative paths.
#[must_use]
pub fn status_for_cwd(cwd: &Path) -> Option<RepoStatus> {
    let canon = cwd.canonicalize().ok()?;
    #[cfg(feature = "daemon")]
    if std::env::var_os("CHEVRON_NO_DAEMON").is_none() {
        if let Some(s) = client::try_query(&canon) {
            return Some(s);
        }
        // Miss. Inline-compute below to give the current prompt a result,
        // and kick off a detached daemon spawn so the *next* prompt is
        // served from cache. Best-effort, non-blocking.
        client::try_spawn_async();
    }
    let mut repo = git2::Repository::discover(&canon).ok()?;
    Some(RepoStatus::compute(&mut repo))
}
