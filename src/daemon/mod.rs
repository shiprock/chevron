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
pub mod paths;
#[cfg(feature = "daemon")]
pub mod proto;
#[cfg(feature = "daemon")]
pub mod state;
