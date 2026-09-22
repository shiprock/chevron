//! Daemon entry point — set up socket, lock, state actor, and run until
//! told to stop.
//!
//! Shutdown is orderly and has three triggers, all funnelled through one
//! self-pipe: SIGTERM, SIGINT, and the idle watchdog (no client activity
//! and no attached subscribers for `CHEVRON_DAEMON_IDLE_TIMEOUT_MS`,
//! default 30 minutes, `0` disables — so an orphaned daemon retires
//! itself instead of accumulating forever). The coordinator wakes the
//! blocking accept loop, the state actor drains its queue and commits,
//! and the socket and pidfile are removed before exit. Stale files from
//! a hard kill are still tolerated and cleaned up by the next startup.
//!
//! The watchdog doubles as the spool drain: each tick ingests lifecycle
//! events that clients queued while the daemon was down or too busy to
//! ack within the publish budget (see `daemon::spool`).

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use super::{listener, paths, spool, state};

/// Idle timeout default; override with `CHEVRON_DAEMON_IDLE_TIMEOUT_MS`.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(30);

/// Write end of the shutdown self-pipe. Signal handlers may only call
/// async-signal-safe functions; `write(2)` to a pre-opened pipe is the
/// canonical one. Anything that wants the daemon down — SIGTERM,
/// SIGINT, the idle watchdog — writes a byte here and the coordinator
/// thread runs the orderly shutdown exactly once.
static SHUTDOWN_PIPE_W: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_shutdown_signal(_: libc::c_int) {
    request_shutdown();
}

/// Async-signal-safe shutdown request; also callable from ordinary
/// threads (the idle watchdog).
fn request_shutdown() {
    let fd = SHUTDOWN_PIPE_W.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; the fd is a live pipe
        // write end (O_NONBLOCK, so a full pipe can't block a handler).
        unsafe {
            let _ = libc::write(fd, b"x".as_ptr().cast(), 1);
        }
    }
}

/// Create the self-pipe and install SIGTERM/SIGINT handlers. Returns
/// the read end for the coordinator thread.
fn install_signal_handlers() -> io::Result<File> {
    let mut fds = [0i32; 2];
    // SAFETY: fds is a valid two-element out-array for pipe(2).
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both fds were just returned by pipe(2). CLOEXEC keeps the
    // pipe out of any spawned children; O_NONBLOCK on the write end
    // keeps a signal storm from blocking inside the handler.
    unsafe {
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(fds[1], libc::F_GETFL);
        libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    SHUTDOWN_PIPE_W.store(fds[1], Ordering::Relaxed);
    // SAFETY: installing a handler that only calls write(2) — async-
    // signal-safe per POSIX. SA_RESTART keeps unrelated syscalls from
    // spuriously failing with EINTR.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_shutdown_signal as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&raw mut sa.sa_mask);
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGTERM, &raw const sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &raw const sa, std::ptr::null_mut());
    }
    // SAFETY: fds[0] is the pipe read end, owned by no one else.
    Ok(unsafe { File::from_raw_fd(fds[0]) })
}

/// Idle timeout from the environment; `0` disables idle exit.
fn idle_timeout() -> Duration {
    std::env::var("CHEVRON_DAEMON_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(DEFAULT_IDLE_TIMEOUT, Duration::from_millis)
}

/// Main daemon entry. Returns `Ok(())` on clean exit, or `Err` for any
/// unrecoverable startup error.
///
/// Steps:
///   1. Ensure socket directory exists with mode 0700.
///   2. Acquire exclusive flock on `chevrond.lock`. Lock failure (another
///      daemon is running) is reported as a benign `Ok(())` — there's
///      nothing for us to do.
///   3. Unlink any stale socket file and bind the new one.
///   4. Write pidfile.
///   5. Install SIGTERM/SIGINT handlers + shutdown coordinator.
///   6. Spawn the state actor; drain the event spool.
///   7. Spawn the idle watchdog.
///   8. Run the accept loop until a shutdown trigger, then drain the
///      actor (final DB commits) and remove socket + pidfile.
///
/// # Errors
///
/// Returns the first I/O error encountered while creating the socket
/// directory, binding the socket, writing the pidfile, or spawning threads.
pub fn serve() -> io::Result<()> {
    let dir = paths::socket_dir();
    fs::create_dir_all(&dir)?;
    let mut perms = fs::metadata(&dir)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(&dir, perms)?;

    // Exclusive lock. Held for the daemon's lifetime (released when the
    // process exits). Another concurrent daemon attempt would see
    // ErrorKind::WouldBlock here.
    let _lock = match try_lock_exclusive(&paths::lock_path()) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            eprintln!("chevrond: another instance already holds the lock; exiting");
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // Drop any stale socket file from a prior crashed daemon. `UnixListener::bind`
    // refuses to overwrite an existing file.
    let sock = paths::socket_path();
    if sock.exists() {
        let _ = fs::remove_file(&sock);
    }
    let unix_listener = UnixListener::bind(&sock)?;

    write_pidfile(&paths::pid_path())?;

    let hooks = listener::ServeHooks::detached();
    let pipe_r = install_signal_handlers()?;
    spawn_shutdown_coordinator(pipe_r, Arc::clone(&hooks.shutting_down), sock.clone())?;

    // Open the command-history database under the socket dir (chevron-1yn
    // Phase 1). Schema is applied here once at startup; the state actor
    // owns the connection for its lifetime. Schema failures are fatal
    // because the lifecycle subcommand has no way to surface them later.
    let db =
        state::open_db(&dir).map_err(|e| io::Error::other(format!("open commands.db: {e}")))?;
    let (state_tx, state_join) = state::spawn(state::TTL, db)?;

    // Events queued while no daemon was running.
    let _ = spool::ingest(&state_tx);

    spawn_idle_watchdog(&hooks, state_tx.clone())?;

    listener::serve_loop(&unix_listener, &state_tx, &hooks);

    // Orderly drain: the actor processes everything already queued
    // (lifecycle commits included), drops its subscriber senders so
    // relay connections close, then we release the files. The flock
    // releases when the process exits.
    let _ = state_tx.send(state::StateMsg::Shutdown);
    let _ = state_join.join();
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_file(paths::pid_path());
    Ok(())
}

/// One thread parked on the self-pipe. The first byte — signal or idle
/// watchdog — flips the flag and wakes the blocking accept with a
/// throwaway connection. Connect retries briefly: a trigger can land in
/// the window before the accept loop is entered.
fn spawn_shutdown_coordinator(
    pipe_r: File,
    shutting_down: Arc<AtomicBool>,
    sock: std::path::PathBuf,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("chevrond-shutdown".into())
        .spawn(move || {
            let mut byte = [0u8; 1];
            // SAFETY: pipe_r owns a live pipe read end; a 1-byte buffer
            // is a valid read target. Blocks until a trigger writes.
            let n = unsafe { libc::read(pipe_r.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
            if n <= 0 {
                return;
            }
            shutting_down.store(true, Ordering::SeqCst);
            for _ in 0..100 {
                if UnixStream::connect(&sock).is_ok() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })?;
    Ok(())
}

/// Watchdog thread: drains the event spool every tick, and — when idle
/// exit is enabled — retires the daemon once there has been no client
/// connection for the timeout AND no subscriber relay is attached (a
/// relay means a shell is alive and waiting for live-prompt events).
/// FS-watcher traffic deliberately does not count as activity: churn in
/// a watched repo with no shell attached is work nobody consumes.
fn spawn_idle_watchdog(
    hooks: &listener::ServeHooks,
    state_tx: std::sync::mpsc::Sender<state::StateMsg>,
) -> io::Result<()> {
    let timeout = idle_timeout();
    let tick = if timeout.is_zero() {
        Duration::from_mins(1)
    } else {
        (timeout / 4).clamp(Duration::from_millis(50), Duration::from_mins(1))
    };
    let shutting_down = Arc::clone(&hooks.shutting_down);
    let last_activity_ms = Arc::clone(&hooks.last_activity_ms);
    let relay_count = Arc::clone(&hooks.relay_count);
    let started = hooks.started;
    std::thread::Builder::new()
        .name("chevrond-watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(tick);
                if shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                let _ = spool::ingest(&state_tx);
                if timeout.is_zero() {
                    continue;
                }
                let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let idle_ms = now_ms.saturating_sub(last_activity_ms.load(Ordering::SeqCst));
                let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
                if relay_count.load(Ordering::SeqCst) == 0 && idle_ms > timeout_ms {
                    request_shutdown();
                    return;
                }
            }
        })?;
    Ok(())
}

/// Try to acquire an exclusive non-blocking flock on `path`. The file is
/// created if missing. The returned [`File`] must be held for the lifetime
/// of the lock — drop it to release.
///
/// # Errors
///
/// Returns the underlying file-open error, or [`io::ErrorKind::WouldBlock`]
/// if another process already holds the lock.
pub fn try_lock_exclusive(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    // SAFETY: `file` owns a valid fd for the duration of the call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// Read the contents of a pidfile as a `u32`.
///
/// # Errors
///
/// I/O error opening or reading the file, or `InvalidData` if the file
/// content isn't a valid `u32`.
pub fn read_pidfile(path: &Path) -> io::Result<u32> {
    let s = fs::read_to_string(path)?;
    s.trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Send signal 0 to `pid` to test for process existence without affecting it.
/// Returns `true` if the process exists and we have permission to signal it.
#[must_use]
pub fn pid_alive(pid: u32) -> bool {
    // pid_t is i32; PIDs above i32::MAX are nonsense. Reject those rather
    // than passing -1 to kill (which would target the process group).
    let Ok(signed) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: kill() with sig=0 only checks existence and is always safe.
    unsafe { libc::kill(signed, 0) == 0 }
}

/// `chevron daemon stop` — read pidfile, send SIGTERM, wait briefly.
/// Returns a process exit code (0 success, non-zero failure).
#[must_use]
pub fn stop() -> i32 {
    let pid_path = paths::pid_path();
    let Ok(pid) = read_pidfile(&pid_path) else {
        eprintln!(
            "chevrond: not running (no pidfile at {})",
            pid_path.display()
        );
        return 1;
    };
    let Ok(signed) = libc::pid_t::try_from(pid) else {
        eprintln!("chevrond: pidfile contains out-of-range pid {pid}");
        return 1;
    };
    if !pid_alive(pid) {
        eprintln!("chevrond: pid {pid} is not alive; removing stale pidfile");
        let _ = fs::remove_file(&pid_path);
        return 1;
    }
    // SAFETY: signed is in valid pid_t range; SIGTERM is a defined signal.
    unsafe {
        libc::kill(signed, libc::SIGTERM);
    }

    // Wait up to 2 seconds for the process to exit. Poll tightly — the
    // graceful path drains in single-digit milliseconds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if !pid_alive(pid) {
            let _ = fs::remove_file(&pid_path);
            let _ = fs::remove_file(paths::socket_path());
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    eprintln!("chevrond: timed out waiting for pid {pid} to exit");
    1
}

/// `chevron daemon status` — print running state to stdout.
/// Returns 0 if a live daemon is found, 1 otherwise.
#[must_use]
pub fn status() -> i32 {
    let pid_path = paths::pid_path();
    let Ok(pid) = read_pidfile(&pid_path) else {
        println!("chevrond: not running");
        return 1;
    };
    if pid_alive(pid) {
        println!("chevrond: running (pid {pid})");
        0
    } else {
        println!("chevrond: not running (stale pidfile, pid {pid})");
        1
    }
}

fn write_pidfile(path: &Path) -> io::Result<()> {
    let pid = std::process::id();
    let tmp = path.with_extension("pid.tmp");
    fs::write(&tmp, pid.to_string())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn flock_excludes_second_locker() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("test.lock");
        let _first = try_lock_exclusive(&lock).expect("first lock should succeed");
        let second = try_lock_exclusive(&lock);
        assert!(
            matches!(&second, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
            "second lock should report WouldBlock, got {second:?}"
        );
    }

    #[test]
    fn flock_releases_on_drop() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("test.lock");
        drop(try_lock_exclusive(&lock).expect("first lock"));
        try_lock_exclusive(&lock).expect("second lock after drop");
    }

    #[test]
    fn pidfile_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.pid");
        write_pidfile(&path).unwrap();
        let pid = read_pidfile(&path).unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn pid_alive_returns_true_for_self() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn pid_alive_returns_false_for_impossible_pid() {
        // Linux default pid_max is 4_194_304; macOS caps at 99_998. Anything
        // above both is guaranteed ESRCH. Avoid u32::MAX — it casts to -1
        // as pid_t, and kill(-1, _) targets the whole process group instead
        // of returning an error.
        assert!(!pid_alive(1_000_000_000));
    }

    #[test]
    fn read_pidfile_rejects_garbage() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.pid");
        fs::write(&path, "not a number").unwrap();
        assert!(read_pidfile(&path).is_err());
    }
}
