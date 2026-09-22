//! Shared pseudoterminal plumbing for `chevron capture` (output capture)
//! and `chevron host` (the screen-ownership host, chevron-dw5).
//!
//! Both subcommands allocate a PTY, run a child against the slave,
//! propagate window-size changes through a SIGWINCH self-pipe, put the
//! real terminal into raw mode (restored on Drop), and pump bytes with a
//! `poll()` loop. The reusable primitives live here; each caller keeps
//! its own loop because the policies differ — `capture` tees the
//! child's output to a file, `host` is a transparent 1:1 wire.
//!
//! Gated on `any(feature = "daemon", feature = "host")`: `capture` is a
//! `daemon`-feature subcommand and `host` is its own feature, so at
//! least one must be enabled for this module to be needed.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};

/// File descriptor of the PTY master, exposed to the SIGWINCH handler.
/// `-1` sentinel means "no active PTY" (no handler to wake). `AtomicI32`
/// because signal handlers must be async-signal-safe; `AtomicI32::store`
/// is.
pub(crate) static MASTER_FD_FOR_WINCH: AtomicI32 = AtomicI32::new(-1);

/// Write end of the self-pipe used to wake the poll loop out of the
/// kernel when SIGWINCH fires. Signal handlers can't run arbitrary Rust,
/// but writing a single byte to a pipe is async-signal-safe.
pub(crate) static WINCH_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

// ── PTY allocation ──────────────────────────────────────────────────────────

pub(crate) fn openpty_pair() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut master: libc::c_int = 0;
    let mut slave: libc::c_int = 0;
    // SAFETY: openpty writes the two fds and returns 0 on success.
    // Both args are valid `c_int*` locations on our stack.
    let rc = unsafe {
        libc::openpty(
            std::ptr::addr_of_mut!(master),
            std::ptr::addr_of_mut!(slave),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openpty returned 0 → both fds are valid.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: same.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    Ok((master, slave))
}

// ── Terminal helpers ────────────────────────────────────────────────────────

pub(crate) fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty is safe on any int; returns 0/1.
    unsafe { libc::isatty(fd) == 1 }
}

pub(crate) fn get_winsize(fd: RawFd) -> Option<libc::winsize> {
    // SAFETY: ioctl writes a libc::winsize into a stack-local; OK as
    // long as the fd is valid. If not, ioctl returns -1 and we
    // return None.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: ioctl writes a winsize into the &mut local; valid fd or
    // returns -1 (handled below).
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, std::ptr::addr_of_mut!(ws)) };
    if rc == -1 { None } else { Some(ws) }
}

pub(crate) fn set_winsize(fd: RawFd, ws: libc::winsize) {
    // SAFETY: ioctl reads from `ws` which is a valid stack value.
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, std::ptr::addr_of!(ws));
    }
}

/// Save the current termios for `fd` on construction, and restore it
/// on Drop. Puts the fd into raw mode on construction; either way the
/// cooked-mode state from before is restored.
pub(crate) struct TermiosGuard {
    fd: RawFd,
    saved: libc::termios,
}

impl TermiosGuard {
    pub(crate) fn install_raw_mode(fd: RawFd) -> std::io::Result<Self> {
        // SAFETY: termios is a plain C struct; zeroed initialisation
        // is a valid bit pattern that tcgetattr will fully overwrite.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tcgetattr writes a libc::termios via the out
        // parameter; the stack-local is well-aligned.
        if unsafe { libc::tcgetattr(fd, std::ptr::addr_of_mut!(saved)) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = saved;
        // SAFETY: cfmakeraw mutates termios in place; valid usage.
        unsafe { libc::cfmakeraw(std::ptr::addr_of_mut!(raw)) };
        // SAFETY: tcsetattr reads from a valid stack-local termios.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, std::ptr::addr_of!(raw)) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        // Best-effort restoration: ignore errors. If this fails the
        // user's shell is in raw mode, which is bad — but there's
        // nothing actionable we can do from Drop.
        // SAFETY: self.saved was populated from a valid tcgetattr.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, std::ptr::addr_of!(self.saved));
        }
    }
}

// ── SIGWINCH self-pipe ──────────────────────────────────────────────────────

pub(crate) fn pipe_cloexec_nonblocking() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [0, 0];
    // pipe2 with O_CLOEXEC + O_NONBLOCK in one syscall avoids a TOCTOU
    // window where a fork could leak the fd. Available on Linux; on
    // macOS we fall back to pipe + fcntl.
    #[cfg(target_os = "linux")]
    // SAFETY: pipe2 writes two fds into `fds` (length-2 array) and
    // returns 0 on success / -1 with errno.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
    #[cfg(not(target_os = "linux"))]
    // SAFETY: pipe writes two fds and returns 0 on success; fcntl
    // with F_GETFD/F_SETFD/F_GETFL/F_SETFL on a valid fd is safe.
    let rc = unsafe {
        let r = libc::pipe(fds.as_mut_ptr());
        if r == 0 {
            for fd in &fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                libc::fcntl(*fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
                let sflags = libc::fcntl(*fd, libc::F_GETFL);
                libc::fcntl(*fd, libc::F_SETFL, sflags | libc::O_NONBLOCK);
            }
        }
        r
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded → both fds are valid open file descriptors.
    let r = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: same.
    let w = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((r, w))
}

extern "C" fn sigwinch_handler(_signum: libc::c_int) {
    // Only async-signal-safe operations below: AtomicI32 load +
    // libc::write to a pipe (POSIX guarantees write() is AS-safe).
    let w = WINCH_PIPE_WRITE.load(Ordering::SeqCst);
    if w >= 0 {
        let byte: u8 = b'!';
        // SAFETY: writing one byte to a valid pipe fd. Returns count
        // or -1; ignored.
        unsafe {
            libc::write(w, std::ptr::addr_of!(byte).cast(), 1);
        }
    }
}

pub(crate) fn install_sigwinch_handler() {
    // SAFETY: sigaction is a plain C struct; zeroed-out is a valid
    // bit pattern that we then populate field-by-field below.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = sigwinch_handler as *const () as usize;
    sa.sa_flags = libc::SA_RESTART;
    // SAFETY: sigaction with a valid handler function pointer. The
    // stored fds gate the handler's effect: both are reset to -1 when
    // no PTY is active, so the handler becomes a no-op even though the
    // OS-level handler stays installed for the process lifetime.
    unsafe {
        libc::sigemptyset(std::ptr::addr_of_mut!(sa.sa_mask));
        libc::sigaction(libc::SIGWINCH, std::ptr::addr_of!(sa), std::ptr::null_mut());
    }
}

// ── IO write helper ─────────────────────────────────────────────────────────

/// Write all of `buf` to `fd`, retrying on EINTR and short writes.
/// Returns Ok(()) on full success, Err otherwise (caller may drop
/// the error if best-effort is fine).
pub(crate) fn write_all(fd: RawFd, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: write to a valid fd with a slice we own.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        #[allow(clippy::cast_sign_loss)]
        let n = n as usize;
        if n == 0 {
            return Err(std::io::Error::other("write returned 0"));
        }
        buf = &buf[n..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn openpty_returns_a_master_slave_pair_of_distinct_fds() {
        let (m, s) = openpty_pair().unwrap();
        assert!(m.as_raw_fd() >= 0);
        assert!(s.as_raw_fd() >= 0);
        assert_ne!(m.as_raw_fd(), s.as_raw_fd());
    }

    #[test]
    fn termios_guard_restores_mode_on_drop() {
        // Use a freshly-allocated PTY slave as the test target so we
        // don't race with sibling tests or affect the process's real
        // STDIN termios (cargo's parallel test runner caught the
        // latter as a pre-push flake). We verify the PROPERTY that
        // matters — raw mode clears ICANON, restored cooked mode
        // sets it — rather than byte-exact c_lflag equality. macOS
        // sets bits like PENDIN transiently around tcsetattr which
        // makes literal equality flaky on PTY slaves.
        let (_master, slave) = openpty_pair().unwrap();
        let fd = slave.as_raw_fd();

        let read_lflag = |fd: i32| -> libc::tcflag_t {
            // SAFETY: tcgetattr writes a libc::termios via the out
            // parameter on a valid PTY fd.
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            unsafe { libc::tcgetattr(fd, std::ptr::addr_of_mut!(t)) };
            t.c_lflag
        };

        // Cooked-mode PTY slave should have ICANON set.
        assert!(
            read_lflag(fd) & libc::ICANON != 0,
            "fresh PTY slave should be in cooked mode"
        );
        {
            let _guard = TermiosGuard::install_raw_mode(fd).unwrap();
            // In raw mode now: ICANON must be clear.
            assert!(
                read_lflag(fd) & libc::ICANON == 0,
                "raw mode should clear ICANON"
            );
        }
        // After drop the guard restored cooked mode.
        assert!(
            read_lflag(fd) & libc::ICANON != 0,
            "guard's Drop should restore ICANON"
        );
    }
}
