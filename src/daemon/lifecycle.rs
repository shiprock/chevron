//! Daemon entry point — set up socket, lock, state actor, and run forever.
//!
//! Phase 1 has no signal handling and no idle timeout: the daemon exits when
//! the kernel kills it (default SIGTERM behaviour) or accept fails fatally.
//! Stale socket/pid files left behind are tolerated and cleaned up by the
//! next startup.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

use super::{listener, paths, state};

/// Main daemon entry. Returns `Ok(())` on clean exit (rare), or `Err` for
/// any unrecoverable startup error.
///
/// Steps:
///   1. Ensure socket directory exists with mode 0700.
///   2. Acquire exclusive flock on `chevrond.lock`. Lock failure (another
///      daemon is running) is reported as a benign `Ok(())` — there's
///      nothing for us to do.
///   3. Unlink any stale socket file and bind the new one.
///   4. Write pidfile.
///   5. Spawn the state actor.
///   6. Run the accept loop forever.
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

    let (state_tx, _state_join) = state::spawn(state::TTL)?;

    listener::serve_loop(&unix_listener, &state_tx);

    // Reached only if accept() returns None (listener dropped). Clean up.
    let _ = fs::remove_file(&sock);
    let _ = fs::remove_file(paths::pid_path());
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

    // Wait up to 2 seconds for the process to exit. Poll cheaply.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if !pid_alive(pid) {
            let _ = fs::remove_file(&pid_path);
            let _ = fs::remove_file(paths::socket_path());
            return 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
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
