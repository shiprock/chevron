//! `chevron host` — Stage 0 of the screen-ownership epic (chevron-dw5):
//! a transparent PTY host.
//!
//! Runs the user's shell (`$SHELL`, or an explicit argv after `--`)
//! inside a pseudoterminal and forwards bytes 1:1 between the real
//! terminal and the child. NO compositing, NO status region, NO
//! terminal emulation — chevron is a transparent wire here. `$TERM` is
//! left UNCHANGED so the inner shell negotiates against the *real*
//! emulator's capabilities; that is exactly why vim, htop, inline
//! images, and mouse reporting survive a `host` session untouched.
//!
//! It proves the plumbing for the screen-ownership epic: PTY allocation,
//! raw mode with a Drop-guarded restore, SIGWINCH → `TIOCSWINSZ`
//! propagation, child `setsid` + `TIOCSCTTY`, and exit-code mirroring.
//! Stages 1–2 add a status region and live-zone compositing on top of
//! this loop; Stage 2 is where the `shell.rs` DSR/transient machinery
//! gets deleted (chevron is the cursor's owner, so it never needs to
//! query for it).
//!
//! The reusable PTY primitives — `openpty`, winsize, the raw-mode guard,
//! the SIGWINCH self-pipe, `write_all` — live in [`crate::pty`], shared
//! with `chevron capture`. This module keeps only the host-specific
//! transparent-passthrough loop.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::atomic::Ordering;

use crate::pty::{
    MASTER_FD_FOR_WINCH, TermiosGuard, WINCH_PIPE_WRITE, get_winsize, install_sigwinch_handler,
    is_tty, openpty_pair, pipe_cloexec_nonblocking, set_winsize, write_all,
};

/// Dispatch `chevron host …`. Returns the child's exit code (or 1 if we
/// could not even start it).
#[must_use]
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("--help") {
        eprintln!("Usage: chevron host [-- <cmd> [args...]]");
        eprintln!();
        eprintln!("Runs <cmd> (default: $SHELL) inside a PTY with transparent 1:1");
        eprintln!("passthrough. Stage 0 of the screen-ownership epic (chevron-dw5)");
        eprintln!("— no compositing yet. $TERM is left unchanged, so full-screen");
        eprintln!("apps (vim, htop, less) behave exactly as without it.");
        return 0;
    }

    let cmd_args = resolve_command(args);
    match run_pty(&cmd_args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("chevron host: {e}");
            1
        }
    }
}

/// Resolve the argv to run: an explicit command after an optional `--`,
/// else `$SHELL`, else `/bin/sh`. Always returns a non-empty vector.
fn resolve_command(args: &[String]) -> Vec<String> {
    let rest = if args.first().map(String::as_str) == Some("--") {
        &args[1..]
    } else {
        args
    };
    if rest.is_empty() {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        vec![shell]
    } else {
        rest.to_vec()
    }
}

/// Allocate a PTY, run `cmd_args` in the child with the slave as its
/// stdio, and pump bytes between the real terminal and the master until
/// the child exits. Returns the child's exit code.
fn run_pty(cmd_args: &[String]) -> std::io::Result<i32> {
    // 1. PTY pair.
    let (master, slave) = openpty_pair()?;
    let master_fd = master.as_raw_fd();

    // 2. Seed the slave's window size from the real terminal (if a TTY).
    if let Some(winsz) = get_winsize(libc::STDIN_FILENO) {
        set_winsize(slave.as_raw_fd(), winsz);
    }

    // 3. SIGWINCH self-pipe + handler so resizes propagate to the master.
    let (winch_r, winch_w) = pipe_cloexec_nonblocking()?;
    MASTER_FD_FOR_WINCH.store(master_fd, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(winch_w.as_raw_fd(), Ordering::SeqCst);
    install_sigwinch_handler();

    // 4. Real stdin → raw mode (child sees keystrokes byte-by-byte). The
    //    guard restores cooked mode on drop, including on unwind. Only
    //    when stdin is a TTY (running `host` under a pipe leaves it cooked).
    let _termios_guard = if is_tty(libc::STDIN_FILENO) {
        Some(TermiosGuard::install_raw_mode(libc::STDIN_FILENO)?)
    } else {
        None
    };

    // 5. Spawn the child holding the slave as stdin/stdout/stderr. The
    //    scoped block drops every parent-side slave fd at spawn time, so
    //    the child is the sole slave holder and its exit hangs up the
    //    master (the loop's only exit). Flattening this block reintroduces
    //    the Linux master-never-EOFs hang fixed in capture.rs (chevron-alj).
    let mut child = {
        let mut cmd = std::process::Command::new(&cmd_args[0]);
        cmd.args(&cmd_args[1..]);
        cmd.stdin(Stdio::from(slave.try_clone()?));
        cmd.stdout(Stdio::from(slave.try_clone()?));
        cmd.stderr(Stdio::from(slave));

        // SAFETY: pre_exec runs in the child between fork and exec. setsid()
        // and ioctl(TIOCSCTTY) are async-signal-safe and touch no Rust-side
        // state; after exec the process image is replaced regardless.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Make the slave (now fd 0 post-dup2) the controlling tty.
                // TIOCSCTTY is u32 on macOS, c_ulong on Linux; the
                // conversion is a no-op on Linux (allow the lint there).
                #[allow(clippy::useless_conversion)]
                let request: libc::c_ulong = libc::TIOCSCTTY.into();
                if libc::ioctl(0, request, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        cmd.spawn()?
    };

    // 6. Pump until the child exits.
    host_io_loop(master_fd, winch_r.as_raw_fd())?;

    // 7. Reap and disarm the handler.
    let status = child.wait()?;
    MASTER_FD_FOR_WINCH.store(-1, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(-1, Ordering::SeqCst);

    // 8. Mirror the child's exit code (128+signal if killed).
    let exit_code = if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    };
    Ok(exit_code)
}

/// `poll()` loop: master → stdout, real stdin → master, winch self-pipe
/// → propagate size. Breaks when the master EOFs/HUPs (child gone).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn host_io_loop(master_fd: RawFd, winch_fd: RawFd) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
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
        // SAFETY: poll over three valid pollfds with an infinite timeout.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }

        // SIGWINCH: drain the pipe, re-query the real terminal, propagate.
        if fds[2].revents & libc::POLLIN != 0 {
            let mut drain = [0u8; 16];
            // SAFETY: reading up to 16 bytes from a valid fd into a stack buf.
            unsafe {
                libc::read(winch_fd, drain.as_mut_ptr().cast(), drain.len());
            }
            if let Some(winsz) = get_winsize(libc::STDIN_FILENO) {
                set_winsize(master_fd, winsz);
            }
        }

        // Real stdin → child PTY.
        if fds[1].revents & libc::POLLIN != 0 {
            // SAFETY: read into our stack buffer; len is well-defined.
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                let _ = write_all(master_fd, &buf[..n as usize]);
            } else if n == 0 {
                // EOF on real stdin: stop polling it, keep draining master.
                fds[1].events = 0;
            }
        }

        // Child PTY → real stdout.
        if fds[0].revents & libc::POLLIN != 0 {
            // SAFETY: same shape as the stdin read above.
            let n = unsafe { libc::read(master_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                let _ = write_all(libc::STDOUT_FILENO, &buf[..n as usize]);
            } else {
                // EOF/EIO: child exited and the slave closed.
                break;
            }
        }
        if fds[0].revents & libc::POLLHUP != 0 {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_command_strips_leading_double_dash() {
        let args = vec!["--".to_string(), "vim".to_string(), "a.txt".to_string()];
        assert_eq!(resolve_command(&args), vec!["vim", "a.txt"]);
    }

    #[test]
    fn resolve_command_without_dashes_runs_args_verbatim() {
        let args = vec!["htop".to_string()];
        assert_eq!(resolve_command(&args), vec!["htop"]);
    }

    #[test]
    fn resolve_command_empty_falls_back_to_a_shell() {
        // SAFETY: single-threaded test; SHELL set then removed.
        unsafe { std::env::set_var("SHELL", "/usr/bin/fish") };
        assert_eq!(resolve_command(&[]), vec!["/usr/bin/fish"]);
        unsafe { std::env::remove_var("SHELL") };
        // With SHELL unset it must still yield a non-empty command.
        assert!(!resolve_command(&[]).is_empty());
    }

    #[test]
    fn help_flag_exits_zero() {
        assert_eq!(run(&["--help".to_string()]), 0);
    }

    #[test]
    fn host_runs_a_command_and_mirrors_its_exit_code() {
        // Non-TTY stdin in the test runner → no raw mode; the child's
        // slave PTY is its stdio. /bin/sh -c 'exit 7' must surface as 7.
        let code = run(&[
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ]);
        assert_eq!(code, 7);
    }
}
