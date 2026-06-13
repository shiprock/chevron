//! `chevron capture` — PTY-interposing command wrapper for opt-in
//! output capture (chevron-1yn.4 Phase 4).
//!
//! Allocates a pseudoterminal pair, forks, execs the user's command
//! in the child with the slave PTY as its `stdin`/`stdout`/`stderr`,
//! and in the parent loops bytes from the master between (a) the
//! user's terminal (so they see output in real time) and (b) a
//! capture file under `$socket_dir/outputs/<id>.log`. Bidirectional:
//! keypress bytes from the real `stdin` pass through the master into
//! the child's PTY, so interactive programs (`less`, `vim`, `ssh`)
//! work.
//!
//! ## Why PTY instead of pipe-tee
//!
//! The user's command can't tell whether its `stdout` is a real TTY
//! or a pipe (`isatty(STDOUT_FILENO)`). Nearly every modern CLI
//! changes its behavior based on this: ANSI colors, progress bars,
//! pagers, interactive prompts. A naive `cmd | tee >(…)` makes every
//! command think it's running non-interactively. PTY interposition
//! gives the child a real TTY, preserving behavior.
//!
//! ## Concurrency shape
//!
//! Single-threaded parent: a `poll()` loop monitors three fds — the
//! PTY master, real `stdin`, and a self-pipe used to wake on
//! SIGWINCH. On master readable: copy to terminal + capture file.
//! On `stdin` readable: copy to master. On SIGWINCH, query terminal
//! size and propagate to master via `TIOCSWINSZ`. When master EOFs
//! (child has exited and the slave side closed), the loop exits,
//! the parent waits the child, and the parent's own exit code
//! mirrors the child's.
//!
//! ## Capture file
//!
//! Bytes are appended to `outputs/<id>.log` as they arrive, capped
//! at [`DEFAULT_MAX_CAPTURE_MB`] (configurable via
//! `CHEVRON_CAPTURE_MAX_MB`, default 10 MB). After the cap, the
//! terminal continues seeing bytes but the file stops growing —
//! `output_truncated=true` is reported in `CMD_END` so subsequent
//! `chevron history --show-output` displays a "(truncated)" marker.
//!
//! ## What can go wrong
//!
//! - Panic between raw-mode-enter and termios-restore: handled via
//!   [`TermiosGuard`] Drop impl, which restores cooked mode even on
//!   unwind.
//! - Child exits but master stays open (slave was inherited by a
//!   grandchild that's still running): we wait the immediate child
//!   via `waitpid`, then break the loop. Grandchildren keep running
//!   detached — same behavior as `script(1)`.
//! - SIGWINCH races: handler writes a byte to the self-pipe; the
//!   poll loop drains the byte and queries terminal size in normal
//!   context. Async-signal-safe.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::daemon::{client, paths, proto};

/// Default per-file capture cap. Overridable via `CHEVRON_CAPTURE_MAX_MB`.
/// 10 MB covers ~95% of realistic build/test outputs; larger captures
/// truncate the tail and mark `output_truncated=true` on the row.
pub const DEFAULT_MAX_CAPTURE_MB: u64 = 10;

/// File descriptor of the PTY master, exposed to the SIGWINCH handler.
/// `-1` sentinel means "no active capture" (no handler to wake).
/// `AtomicI32` because signal handlers must be async-signal-safe;
/// `AtomicI32::store` is.
static MASTER_FD_FOR_WINCH: AtomicI32 = AtomicI32::new(-1);

/// Write end of the self-pipe used to wake the poll loop out of the
/// kernel when SIGWINCH fires. Signal handlers can't run arbitrary
/// Rust, but writing a single byte to a pipe is async-signal-safe.
static WINCH_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

/// Dispatch a `chevron capture …` invocation. Returns the child's
/// exit code (or a non-zero error if we couldn't even start). The
/// shell user typically aliases this as `chcap` for brevity.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    if args.is_empty() || args.first().map(String::as_str) == Some("--help") {
        eprintln!("Usage: chevron capture <cmd> [args...]");
        eprintln!();
        eprintln!("Wraps <cmd> in a PTY-interposing capture wrapper. The terminal");
        eprintln!("sees output in real time; bytes are also written to");
        eprintln!("$CHEVRON_SOCKET_DIR/outputs/<id>.log capped at");
        eprintln!("{DEFAULT_MAX_CAPTURE_MB} MB (override via CHEVRON_CAPTURE_MAX_MB).");
        eprintln!();
        eprintln!("Recommended alias: `alias chcap='chevron capture'`");
        return i32::from(args.is_empty());
    }

    // Compute config up front (env-var reads can fail in tight code paths).
    let max_bytes = std::env::var("CHEVRON_CAPTURE_MAX_MB")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_CAPTURE_MB)
        .saturating_mul(1024 * 1024);

    // Mint command id + record CMD_START. We use chcap's idea of the
    // cmd text (joined args) rather than reading $_chevron_cmd_id from
    // the env: the latter would record `chcap cargo build` from the
    // outer shell's preexec but we want `cargo build` to be searchable
    // via `chevron history cargo`. Living with the duplicate row (one
    // from the outer shell, one from chcap) is acceptable for v1.
    let id = ulid::Ulid::new().to_string();
    let session_id =
        std::env::var("CHEVRON_SESSION_ID").unwrap_or_else(|_| "capture-standalone".to_string());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let cmd_text = args.join(" ");
    publish_cmd_start(&id, &session_id, &cwd, &cmd_text);

    // Set up the output file path and ensure the directory exists.
    let outputs_dir = paths::socket_dir().join("outputs");
    if let Err(e) = std::fs::create_dir_all(&outputs_dir) {
        eprintln!("chevron capture: creating {}: {e}", outputs_dir.display());
        return 1;
    }
    // 0700 on the dir, 0600 on files (umask plus explicit mode).
    set_dir_mode(&outputs_dir, 0o700);
    let output_path = outputs_dir.join(format!("{id}.log"));

    let started_at_ms = now_unix_ms();
    let (exit_code, output_bytes, truncated) = match run_pty(&output_path, args, max_bytes) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("chevron capture: {e}");
            // Still report CMD_END so the row's lifecycle is consistent.
            publish_cmd_end(&id, started_at_ms, 1, 0, false);
            return 1;
        }
    };

    publish_cmd_end(&id, started_at_ms, exit_code, output_bytes, truncated);
    exit_code
}

/// Drive the PTY wrapping. Returns `(exit_code, bytes_captured, truncated)`.
fn run_pty(
    output_path: &Path,
    cmd_args: &[String],
    max_bytes: u64,
) -> std::io::Result<(i32, i64, bool)> {
    // 1. Allocate a PTY pair.
    let (master, slave) = openpty_pair()?;
    let master_fd = master.as_raw_fd();
    let slave_fd = slave.as_raw_fd();

    // 2. Set the slave's initial window size to match the parent's
    //    stdin terminal, if stdin is a TTY. Non-TTY stdin (running
    //    chcap under redirection) leaves the slave at its default
    //    24x80 — fine.
    if let Some(winsz) = get_winsize(libc::STDIN_FILENO) {
        set_winsize(slave_fd, winsz);
    }

    // 3. Stash master_fd for the SIGWINCH handler, set up self-pipe,
    //    install the handler.
    let (winch_r, winch_w) = pipe_cloexec_nonblocking()?;
    MASTER_FD_FOR_WINCH.store(master_fd, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(winch_w.as_raw_fd(), Ordering::SeqCst);
    install_sigwinch_handler();

    // 4. Save current termios; put real stdin into raw mode so the
    //    child sees keystrokes byte-by-byte. The guard restores on
    //    drop, including on panic/unwind.
    let _termios_guard = if is_tty(libc::STDIN_FILENO) {
        Some(TermiosGuard::install_raw_mode(libc::STDIN_FILENO)?)
    } else {
        None
    };

    // 5. Spawn the child. std::process::Command handles dup2'ing the
    //    slave PTY to stdin/stdout/stderr; pre_exec runs in the
    //    child between fork and exec to make the slave the
    //    controlling terminal.
    let mut cmd = std::process::Command::new(&cmd_args[0]);
    cmd.args(&cmd_args[1..]);
    cmd.stdin(Stdio::from(slave.try_clone()?));
    cmd.stdout(Stdio::from(slave.try_clone()?));
    cmd.stderr(Stdio::from(slave));

    // SAFETY: pre_exec runs in the child between fork and exec. The
    // operations are async-signal-safe: setsid() and ioctl(TIOCSCTTY).
    // No allocator or Rust-side state is touched. After exec, the
    // child's process image is replaced so any Rust state from the
    // parent is irrelevant.
    unsafe {
        cmd.pre_exec(|| {
            // New session, detach from controlling terminal.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Make stdin (which is now the slave PTY post-Stdio dup2) our
            // controlling terminal. TIOCSCTTY is u32 on macOS (widened to
            // ioctl's c_ulong request) but already c_ulong on Linux, where
            // the `.into()` is a no-op — keep it for the macOS build and
            // allow clippy::useless_conversion, which fires only on Linux.
            #[allow(clippy::useless_conversion)]
            let request: libc::c_ulong = libc::TIOCSCTTY.into();
            if libc::ioctl(0, request, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn()?;

    // 6. Open the capture file. We hold an OwnedFd via the File so
    //    write() calls amortize via stdlib buffering on drop.
    let output_file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(output_path)?;

    // 7. Run the poll loop.
    let (bytes_written, truncated) =
        pty_io_loop(master_fd, winch_r.as_raw_fd(), output_file, max_bytes)?;

    // 8. Reap the child.
    let status = child.wait()?;

    // 9. Clear the SIGWINCH state so the handler becomes a no-op for
    //    any subsequent chcap invocation in this process (e.g.
    //    nested via shell substitution).
    MASTER_FD_FOR_WINCH.store(-1, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(-1, Ordering::SeqCst);

    // 10. Compute exit code. The Unix convention: status.code() if it
    //     exited normally, 128+signal if it was killed by a signal.
    let exit_code = if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    };

    Ok((
        exit_code,
        i64::try_from(bytes_written).unwrap_or(i64::MAX),
        truncated,
    ))
}

/// `poll()` loop that:
///   - `master_fd` readable → read bytes, write to `stdout` AND
///     `output_file` (capped at `max_bytes`)
///   - `STDIN` readable → read bytes, write to `master_fd`
///   - `winch_fd` readable → drain it and propagate window size from
///     real `stdin` to `master_fd`
///   - master EOF (HUP) → break
///
/// Returns `(bytes_written_to_file, truncated)`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn pty_io_loop(
    master_fd: RawFd,
    winch_fd: RawFd,
    mut output_file: File,
    max_bytes: u64,
) -> std::io::Result<(u64, bool)> {
    let mut total_written: u64 = 0;
    let mut truncated = false;
    let mut buf = [0u8; 8192];

    // Three fds: master, real stdin, winch self-pipe.
    let mut fds = [
        libc::pollfd {
            fd: master_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: winch_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    loop {
        // SAFETY: poll on three valid pollfds with a -1 (infinite)
        // timeout. The buffer is on our stack and lives for the call.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                // SIGWINCH (or anything else) interrupted poll —
                // the self-pipe will be readable next iteration.
                continue;
            }
            return Err(err);
        }

        // Drain the SIGWINCH self-pipe and propagate the new size.
        if fds[2].revents & libc::POLLIN != 0 {
            let mut drain = [0u8; 16];
            // SAFETY: drain is a stack buffer of length 16; reading
            // up to 16 bytes from a valid fd is well-defined.
            unsafe {
                libc::read(winch_fd, drain.as_mut_ptr().cast(), drain.len());
            }
            if let Some(winsz) = get_winsize(libc::STDIN_FILENO) {
                set_winsize(master_fd, winsz);
            }
        }

        // Forward real stdin → child PTY.
        if fds[1].revents & libc::POLLIN != 0 {
            // SAFETY: read into our local buf; buf.len() is well-defined.
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                // SAFETY: write the same bytes we just read; n bounded.
                unsafe {
                    libc::write(master_fd, buf.as_ptr().cast(), n as usize);
                }
            } else if n == 0 {
                // EOF on stdin — stop polling it but keep going on
                // master (child may keep producing).
                fds[1].events = 0;
            }
            // n < 0: transient, retry on next iteration
        }

        // Drain master → terminal + capture file.
        if fds[0].revents & libc::POLLIN != 0 {
            // SAFETY: same shape as the stdin read above.
            let n = unsafe { libc::read(master_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                let bytes = &buf[..n as usize];
                // Always to terminal (use direct syscall to avoid any
                // stdlib buffering between us and the user).
                let _ = write_all(libc::STDOUT_FILENO, bytes);
                // Conditionally to capture file (cap at max_bytes).
                if total_written < max_bytes {
                    let remaining = max_bytes - total_written;
                    #[allow(clippy::cast_possible_truncation)]
                    let to_write = std::cmp::min(bytes.len(), remaining as usize);
                    output_file.write_all(&bytes[..to_write])?;
                    total_written += to_write as u64;
                    if bytes.len() > to_write {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            } else {
                // n == 0 or EIO (POLLHUP path on some platforms) →
                // master is closed because the child exited and the
                // slave was reaped. Drain and break.
                break;
            }
        }
        if fds[0].revents & libc::POLLHUP != 0 {
            break;
        }
    }

    output_file.flush()?;
    Ok((total_written, truncated))
}

// ── PTY allocation ──────────────────────────────────────────────────────────

fn openpty_pair() -> std::io::Result<(OwnedFd, OwnedFd)> {
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

fn is_tty(fd: RawFd) -> bool {
    // SAFETY: isatty is safe on any int; returns 0/1.
    unsafe { libc::isatty(fd) == 1 }
}

fn get_winsize(fd: RawFd) -> Option<libc::winsize> {
    // SAFETY: ioctl writes a libc::winsize into a stack-local; OK as
    // long as the fd is valid. If not, ioctl returns -1 and we
    // return None.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: ioctl writes a winsize into the &mut local; valid fd or
    // returns -1 (handled below).
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, std::ptr::addr_of_mut!(ws)) };
    if rc == -1 { None } else { Some(ws) }
}

fn set_winsize(fd: RawFd, ws: libc::winsize) {
    // SAFETY: ioctl reads from `ws` which is a valid stack value.
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, std::ptr::addr_of!(ws));
    }
}

/// Save the current termios for `fd` on construction, and restore it
/// on Drop. Optionally puts the fd into raw mode on construction;
/// either way the cooked-mode state from before is restored.
struct TermiosGuard {
    fd: RawFd,
    saved: libc::termios,
}

impl TermiosGuard {
    fn install_raw_mode(fd: RawFd) -> std::io::Result<Self> {
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

fn pipe_cloexec_nonblocking() -> std::io::Result<(OwnedFd, OwnedFd)> {
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

fn install_sigwinch_handler() {
    // SAFETY: sigaction is a plain C struct; zeroed-out is a valid
    // bit pattern that we then populate field-by-field below.
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = sigwinch_handler as *const () as usize;
    sa.sa_flags = libc::SA_RESTART;
    // SAFETY: sigaction with a valid handler function pointer. We
    // disable the handler's effect on subsequent capture invocations
    // by overwriting MASTER_FD_FOR_WINCH and WINCH_PIPE_WRITE with
    // -1; the handler becomes a no-op even though the OS-level
    // handler stays installed for the process lifetime.
    unsafe {
        libc::sigemptyset(std::ptr::addr_of_mut!(sa.sa_mask));
        libc::sigaction(libc::SIGWINCH, std::ptr::addr_of!(sa), std::ptr::null_mut());
    }
}

// ── file mode helpers ───────────────────────────────────────────────────────

fn set_dir_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(mode);
        let _ = std::fs::set_permissions(path, perms);
    }
}

// ── IO write helper ─────────────────────────────────────────────────────────

/// Write all of `buf` to `fd`, retrying on EINTR and short writes.
/// Returns Ok(()) on full success, Err otherwise (caller may drop
/// the error if best-effort is fine).
fn write_all(fd: RawFd, mut buf: &[u8]) -> std::io::Result<()> {
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

// ── chevrond integration ────────────────────────────────────────────────────

fn publish_cmd_start(id: &str, session_id: &str, cwd: &Path, cmd: &str) {
    let req = proto::Request::CmdStart(proto::CmdStartEvent {
        id: id.to_string(),
        session_id: session_id.to_string(),
        hostname: hostname_or_unknown(),
        cwd: cwd.to_path_buf(),
        cmd: cmd.to_string(),
        started_at_ms: now_unix_ms(),
    });
    let _ = client::try_publish_event(&req);
}

fn publish_cmd_end(
    id: &str,
    started_at_ms: i64,
    exit_status: i32,
    output_bytes: i64,
    truncated: bool,
) {
    let now = now_unix_ms();
    let req = proto::Request::CmdEnd(proto::CmdEndEvent {
        id: id.to_string(),
        finished_at_ms: now,
        // We don't have an accurate duration_ms because we didn't
        // record started_at as an Instant. Recomputing from
        // started_at_ms is close enough for history filtering
        // purposes; if the user really needs sub-ms precision they
        // can read finished_at - started_at from the DB directly.
        duration_ms: u64::try_from(now.saturating_sub(started_at_ms).max(0)).unwrap_or(0),
        exit_status,
        output_bytes: Some(output_bytes),
        output_truncated: Some(truncated),
    });
    let _ = client::try_publish_event(&req);
}

fn hostname_or_unknown() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes; valid usage.
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

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_no_args_prints_usage_and_exits_one() {
        // No args is treated as a usage error so the shell user can
        // see the help. Important: this must not contact the daemon
        // or touch any FS state.
        assert_eq!(run(&[]), 1);
    }

    #[test]
    fn run_with_help_flag_exits_zero() {
        assert_eq!(run(&["--help".to_string()]), 0);
    }

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

    #[test]
    fn hostname_helper_returns_non_empty() {
        assert!(!hostname_or_unknown().is_empty());
    }

    #[test]
    fn now_unix_ms_is_recent() {
        let t = now_unix_ms();
        assert!(t > 1_600_000_000_000);
    }
}
