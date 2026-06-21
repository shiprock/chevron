//! `chevron host` — the screen-ownership host (epic chevron-dw5).
//!
//! Runs the user's shell (`$SHELL`, or an explicit argv after `--`)
//! inside a pseudoterminal and forwards bytes between the real terminal
//! and the child. Two modes:
//!
//! - **Stage 0 (default):** a transparent 1:1 wire. NO compositing, NO
//!   emulation. `$TERM` is left UNCHANGED so the inner shell negotiates
//!   against the *real* emulator's capabilities — that is why vim, htop,
//!   inline images, and mouse reporting survive untouched.
//! - **Stage 1 (`--status`):** reserves the top terminal row for a
//!   chevron-owned status bar via a DECSTBM scroll region. The shell
//!   scrolls in the rows below; the bar (a live clock + geometry) is
//!   redrawn on a timer and survives the shell's scrolling because it
//!   sits OUTSIDE the scroll region. Full-screen apps that switch to the
//!   alternate screen (vim/htop/less) suspend the bar and get the whole
//!   screen; it returns on exit.
//!
//! Known Stage-1 limits (a real fix is Stage 2's grid emulator, which
//! parses sequences instead of injecting between them): a `clear` or an
//! absolute home escape from the shell can paint over the reserved row
//! until the next redraw, and a child escape sequence split across two
//! `read()`s could be interleaved with a bar redraw. Both are cosmetic.
//!
//! The reusable PTY primitives — `openpty`, winsize, the raw-mode guard,
//! the SIGWINCH self-pipe, `write_all` — live in [`crate::pty`], shared
//! with `chevron capture`.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::pty::{
    MASTER_FD_FOR_WINCH, TermiosGuard, WINCH_PIPE_WRITE, get_winsize, install_sigwinch_handler,
    is_tty, openpty_pair, pipe_cloexec_nonblocking, set_winsize, write_all,
};

/// Redraw cadence for the Stage-1 status bar.
const STATUS_REDRAW: Duration = Duration::from_millis(500);

/// Dispatch `chevron host …`. Returns the child's exit code (or 1 if we
/// could not even start it).
#[must_use]
pub fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("--help") {
        eprintln!("Usage: chevron host [--status] [-- <cmd> [args...]]");
        eprintln!();
        eprintln!("Runs <cmd> (default: $SHELL) inside a PTY (screen-ownership");
        eprintln!("epic chevron-dw5). Default is transparent 1:1 passthrough.");
        eprintln!();
        eprintln!("  --status   reserve the top row for a chevron-owned status bar");
        eprintln!("             (Stage 1); the shell scrolls below it.");
        return 0;
    }

    let mut requested_status = false;
    let mut rest: &[String] = args;
    if rest.first().map(String::as_str) == Some("--status") {
        requested_status = true;
        rest = &rest[1..];
    }

    let cmd_args = resolve_command(rest);
    match run_pty(&cmd_args, requested_status) {
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
/// stdio, optionally reserve the top row for a status bar, and pump
/// bytes until the child exits. Returns the child's exit code.
fn run_pty(cmd_args: &[String], requested_status: bool) -> std::io::Result<i32> {
    // 1. PTY pair.
    let (master, slave) = openpty_pair()?;
    let master_fd = master.as_raw_fd();

    // 2. Seed the slave's window size from the real terminal (if a TTY).
    let winsz = get_winsize(libc::STDIN_FILENO);
    if let Some(w) = winsz {
        set_winsize(slave.as_raw_fd(), w);
    }

    // 3. SIGWINCH self-pipe + handler so resizes propagate to the master.
    let (winch_r, winch_w) = pipe_cloexec_nonblocking()?;
    MASTER_FD_FOR_WINCH.store(master_fd, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(winch_w.as_raw_fd(), Ordering::SeqCst);
    install_sigwinch_handler();

    // 4. Real stdin → raw mode. The guard restores cooked mode on drop.
    //    Only when stdin is a TTY (under a pipe it stays cooked).
    let stdin_is_tty = is_tty(libc::STDIN_FILENO);
    let _termios_guard = if stdin_is_tty {
        Some(TermiosGuard::install_raw_mode(libc::STDIN_FILENO)?)
    } else {
        None
    };

    // 5. Stage 1: reserve the top row for the status bar (only with a
    //    real TTY and a known size). The guard tears the region back
    //    down on any exit path, including unwind.
    let host = hostname();
    let status = requested_status && stdin_is_tty && winsz.is_some();
    let _status_guard = if status {
        let (cols, rows) = winsz.map_or((80, 24), |w| (w.ws_col, w.ws_row));
        set_scroll_region(rows);
        // Drop the shell below the bar so its first prompt lands at row 2.
        let _ = write_all(libc::STDOUT_FILENO, b"\x1b[2;1H");
        draw_bar(cols, rows, &host, false, None, None);
        Some(StatusGuard)
    } else {
        None
    };

    // 6. Spawn the child holding the slave as stdin/stdout/stderr. The
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

    // 7. Pump until the child exits.
    host_io_loop(master_fd, winch_r.as_raw_fd(), status, &host)?;

    // 8. Reap and disarm the handler.
    let status_code = child.wait()?;
    MASTER_FD_FOR_WINCH.store(-1, Ordering::SeqCst);
    WINCH_PIPE_WRITE.store(-1, Ordering::SeqCst);

    // 9. Mirror the child's exit code (128+signal if killed).
    let exit_code = if let Some(code) = status_code.code() {
        code
    } else if let Some(sig) = status_code.signal() {
        128 + sig
    } else {
        1
    };
    Ok(exit_code)
}

/// `poll()` loop: master → stdout, real stdin → master, winch self-pipe
/// → propagate size. With `status`, also redraws the reserved top-row
/// bar on a timer and tracks alternate-screen entry/exit so full-screen
/// apps get the whole screen. Breaks when the master EOFs/HUPs.
// A single poll-dispatch loop reads clearest in one place (cf. `main`'s
// dispatch, which takes the same allow); the OSC/bar bookkeeping pushes it
// just over the line budget.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
fn host_io_loop(
    master_fd: RawFd,
    winch_fd: RawFd,
    status: bool,
    host: &str,
) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let (mut cols, mut rows) =
        get_winsize(libc::STDIN_FILENO).map_or((80, 24), |w| (w.ws_col, w.ws_row));
    let mut alt = false;
    let mut last_draw = Instant::now();
    // OSC 133 command lifecycle (C → running, D;<n> → finished) drives the
    // live command-state field in the bar. This scanner is Stage 2's backbone.
    let mut osc = OscScanner::default();
    let mut events: Vec<OscEvent> = Vec::new();
    let mut running = false;
    let mut last_exit: Option<i32> = None;
    let mut cmd_start: Option<Instant> = None;
    let mut last_dur: Option<Duration> = None;
    // Poll wakes periodically in status mode so the clock stays live even
    // while the shell sits idle; otherwise it blocks indefinitely.
    let timeout: libc::c_int = if status { 250 } else { -1 };

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
        // SAFETY: poll over three valid pollfds.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 3, timeout) };
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
            if let Some(w) = get_winsize(libc::STDIN_FILENO) {
                set_winsize(master_fd, w);
                cols = w.ws_col;
                rows = w.ws_row;
                if status && !alt {
                    set_scroll_region(rows);
                    let dur = if running {
                        cmd_start.map(|s| s.elapsed())
                    } else {
                        last_dur
                    };
                    draw_bar(cols, rows, host, running, last_exit, dur);
                    last_draw = Instant::now();
                }
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
                let chunk = &buf[..n as usize];
                let _ = write_all(libc::STDOUT_FILENO, chunk);
                if status {
                    let now_alt = scan_alt_screen(chunk, alt);
                    if now_alt != alt {
                        alt = now_alt;
                        if alt {
                            // Full-screen app takes over: release the row.
                            reset_scroll_region();
                        } else {
                            // Back to the shell: re-reserve and repaint.
                            set_scroll_region(rows);
                            let dur = if running {
                                cmd_start.map(|s| s.elapsed())
                            } else {
                                last_dur
                            };
                            draw_bar(cols, rows, host, running, last_exit, dur);
                            last_draw = Instant::now();
                        }
                    }
                    // OSC 133: C → command running, D;<n> → finished, exit n.
                    osc.feed(chunk, &mut events);
                    let mut changed = false;
                    for ev in events.drain(..) {
                        match ev {
                            OscEvent::OutputStart => {
                                running = true;
                                cmd_start = Some(Instant::now());
                                changed = true;
                            }
                            OscEvent::CmdEnd(code) => {
                                running = false;
                                last_exit = code;
                                last_dur = cmd_start.take().map(|s| s.elapsed());
                                changed = true;
                            }
                            OscEvent::PromptStart | OscEvent::CmdStart => {}
                        }
                    }
                    if changed && !alt {
                        let dur = if running {
                            cmd_start.map(|s| s.elapsed())
                        } else {
                            last_dur
                        };
                        draw_bar(cols, rows, host, running, last_exit, dur);
                        last_draw = Instant::now();
                    }
                }
            } else {
                // EOF/EIO: child exited and the slave closed.
                break;
            }
        }
        if fds[0].revents & libc::POLLHUP != 0 {
            break;
        }

        // Keep the clock — and a running command's live timer — fresh.
        if status && !alt && last_draw.elapsed() >= STATUS_REDRAW {
            let dur = if running {
                cmd_start.map(|s| s.elapsed())
            } else {
                last_dur
            };
            draw_bar(cols, rows, host, running, last_exit, dur);
            last_draw = Instant::now();
        }
    }
    Ok(())
}

// ── Stage 1 status bar ───────────────────────────────────────────────────────

/// Build the cursor-neutral DECSTBM sequence reserving row 1 (region
/// `2..=rows`). Empty when `rows < 2` (no room to reserve).
///
/// DECSTBM moves the cursor to home (1,1) as a side effect, so the region
/// change is wrapped in DECSC/DECRC (save/restore cursor). That is what
/// keeps the shell's cursor where it was — critical on alt-screen exit
/// (quitting btop/vim), where the terminal has just restored the cursor
/// into the content area and a bare DECSTBM would yank it onto the
/// reserved row, so the next prompt paints over the bar (chevron-dw5.2).
fn scroll_region_seq(rows: u16) -> String {
    if rows < 2 {
        return String::new();
    }
    format!("\x1b7\x1b[2;{rows}r\x1b8")
}

/// Set the DECSTBM scroll region to rows `2..=rows`, reserving row 1.
fn set_scroll_region(rows: u16) {
    let _ = write_all(libc::STDOUT_FILENO, scroll_region_seq(rows).as_bytes());
}

/// Release the scroll region (full screen) — used while an alt-screen app
/// runs. Cursor-neutral for the same reason as [`scroll_region_seq`].
fn reset_scroll_region() {
    let _ = write_all(libc::STDOUT_FILENO, b"\x1b7\x1b[r\x1b8");
}

/// Render the reserved row from the current bar state: save cursor
/// (DECSC), home, reverse video, the padded line, reset attrs, restore
/// cursor (DECRC). Save/restore keeps the child's cursor untouched; row 1
/// is outside the scroll region so this never scrolls the shell's content.
fn draw_bar(
    cols: u16,
    rows: u16,
    host: &str,
    running: bool,
    last_exit: Option<i32>,
    dur: Option<Duration>,
) {
    if cols == 0 {
        return;
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let status = cmd_status_text(running, last_exit, dur);
    let line = bar_line(cols, rows, secs, host, &status);
    let seq = format!("\x1b7\x1b[1;1H\x1b[7m{line}\x1b[0m\x1b8");
    let _ = write_all(libc::STDOUT_FILENO, seq.as_bytes());
}

/// Compose the bar text and pad/truncate to exactly `cols` cells. Content
/// is width-1 characters, so cell width equals `chars().count()`.
fn bar_line(cols: u16, rows: u16, secs: u64, host: &str, status: &str) -> String {
    let width = usize::from(cols);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let label =
        format!(" chevron host   {host}   {status}   {h:02}:{m:02}:{s:02} UTC   {cols}x{rows} ");
    let mut text: String = label.chars().take(width).collect();
    let len = text.chars().count();
    if len < width {
        text.push_str(&" ".repeat(width - len));
    }
    text
}

/// Format the command-status field from the OSC 133 lifecycle state.
fn cmd_status_text(running: bool, last_exit: Option<i32>, dur: Option<Duration>) -> String {
    if running {
        dur.map_or_else(
            || "running".to_string(),
            |d| format!("running {}", fmt_dur(d)),
        )
    } else if let Some(code) = last_exit {
        match (code, dur.map(fmt_dur)) {
            (0, Some(d)) => format!("ok {d}"),
            (0, None) => "ok".to_string(),
            (n, Some(d)) => format!("exit {n} {d}"),
            (n, None) => format!("exit {n}"),
        }
    } else {
        "idle".to_string()
    }
}

/// Human-friendly duration: `350ms`, `1.2s`, `2m05s`.
fn fmt_dur(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if d.as_secs() < 60 {
        format!("{:.1}s", d.as_secs_f64())
    } else {
        let s = d.as_secs();
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// Best-effort hostname for the bar, from the inherited environment.
fn hostname() -> String {
    std::env::var("HOST")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "host".to_string())
}

// ── OSC 133 stream scanner (Stage 2 backbone) ────────────────────────────────

/// Semantic-prompt events parsed from the child's output stream.
enum OscEvent {
    PromptStart,
    CmdStart,
    OutputStart,
    CmdEnd(Option<i32>),
}

#[derive(Default)]
enum OscState {
    #[default]
    Ground,
    Esc,
    Osc,
}

/// Incremental scanner for `ESC ] … BEL` OSC sequences, tolerant of a
/// sequence split across `read()` chunks. Only BEL-terminated OSCs are
/// parsed (chevron emits those); an embedded ESC abandons the current OSC,
/// so an ST-terminated OSC from another program is simply ignored.
#[derive(Default)]
struct OscScanner {
    state: OscState,
    buf: Vec<u8>,
}

impl OscScanner {
    fn feed(&mut self, chunk: &[u8], out: &mut Vec<OscEvent>) {
        for &b in chunk {
            match self.state {
                OscState::Ground => {
                    if b == 0x1b {
                        self.state = OscState::Esc;
                    }
                }
                OscState::Esc => {
                    if b == b']' {
                        self.state = OscState::Osc;
                        self.buf.clear();
                    } else {
                        self.state = OscState::Ground;
                    }
                }
                OscState::Osc => {
                    if b == 0x07 {
                        if let Some(ev) = parse_osc_133(&self.buf) {
                            out.push(ev);
                        }
                        self.state = OscState::Ground;
                        self.buf.clear();
                    } else if b == 0x1b {
                        // ST terminator or a fresh escape: abandon this OSC.
                        self.state = OscState::Esc;
                        self.buf.clear();
                    } else if self.buf.len() < 128 {
                        self.buf.push(b);
                    } else {
                        // Runaway OSC payload — give up on it.
                        self.state = OscState::Ground;
                        self.buf.clear();
                    }
                }
            }
        }
    }
}

/// Parse an OSC payload (bytes between `ESC]` and the terminator) as an
/// OSC 133 marker: `133;A|B|C` or `133;D[;<exit>]`.
fn parse_osc_133(buf: &[u8]) -> Option<OscEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    let mut parts = s.split(';');
    if parts.next()? != "133" {
        return None;
    }
    match parts.next()? {
        "A" => Some(OscEvent::PromptStart),
        "B" => Some(OscEvent::CmdStart),
        "C" => Some(OscEvent::OutputStart),
        "D" => Some(OscEvent::CmdEnd(
            parts.next().and_then(|c| c.parse::<i32>().ok()),
        )),
        _ => None,
    }
}

/// Scan a forwarded chunk for alternate-screen toggles and return the
/// resulting state. Recognises the 1049 / 1047 / 47 private modes; the
/// later of the last enter vs. last exit wins within a chunk.
fn scan_alt_screen(chunk: &[u8], current: bool) -> bool {
    let enters: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
    let exits: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];
    let last_enter = enters.iter().filter_map(|n| last_index_of(chunk, n)).max();
    let last_exit = exits.iter().filter_map(|n| last_index_of(chunk, n)).max();
    match (last_enter, last_exit) {
        (Some(e), Some(x)) => e > x,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => current,
    }
}

/// Index of the last occurrence of `needle` in `hay`, if any.
fn last_index_of(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Restores the terminal when the host exits: release the scroll region,
/// clear the reserved row, and park the cursor at the bottom so the
/// outer shell's prompt resumes cleanly. Re-queries size so a resize
/// mid-session can't strand the cursor off-screen.
struct StatusGuard;

impl Drop for StatusGuard {
    fn drop(&mut self) {
        let rows = get_winsize(libc::STDIN_FILENO).map_or(24, |w| w.ws_row);
        let seq = format!("\x1b[r\x1b[1;1H\x1b[2K\x1b[{rows};1H");
        let _ = write_all(libc::STDOUT_FILENO, seq.as_bytes());
    }
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
        assert!(!resolve_command(&[]).is_empty());
    }

    #[test]
    fn help_flag_exits_zero() {
        assert_eq!(run(&["--help".to_string()]), 0);
    }

    #[test]
    fn host_runs_a_command_and_mirrors_its_exit_code() {
        // Non-TTY stdin in the test runner → no raw mode, no status bar;
        // the child's slave PTY is its stdio. exit 7 must surface as 7.
        let code = run(&[
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ]);
        assert_eq!(code, 7);
    }

    #[test]
    fn status_flag_does_not_change_exit_code() {
        // --status is inert without a TTY (the test runner's stdin is a
        // pipe), so the command still runs and its code still mirrors.
        let code = run(&[
            "--status".to_string(),
            "--".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 3".to_string(),
        ]);
        assert_eq!(code, 3);
    }

    #[test]
    fn bar_line_is_exactly_cols_wide() {
        for cols in [1u16, 10, 40, 80, 200] {
            let text = bar_line(cols, 24, 45_296, "m-217", "ok 1.2s");
            assert_eq!(text.chars().count(), usize::from(cols), "width {cols}");
        }
    }

    #[test]
    fn bar_line_renders_clock_host_and_status() {
        // 45_296 s = 12:34:56 UTC.
        let text = bar_line(120, 24, 45_296, "m-217", "running 3.4s");
        assert!(text.contains("12:34:56"), "got: {text}");
        assert!(text.contains("m-217"));
        assert!(text.contains("running 3.4s"));
        assert!(text.contains("120x24"));
    }

    #[test]
    fn cmd_status_text_covers_running_ok_and_failure() {
        let d = Some(Duration::from_millis(1234));
        assert_eq!(cmd_status_text(true, None, d), "running 1.2s");
        assert_eq!(cmd_status_text(false, Some(0), d), "ok 1.2s");
        assert_eq!(cmd_status_text(false, Some(2), d), "exit 2 1.2s");
        assert_eq!(cmd_status_text(false, Some(0), None), "ok");
        assert_eq!(cmd_status_text(false, None, None), "idle");
    }

    #[test]
    fn fmt_dur_scales_units() {
        assert_eq!(fmt_dur(Duration::from_millis(350)), "350ms");
        assert_eq!(fmt_dur(Duration::from_millis(1234)), "1.2s");
        assert_eq!(fmt_dur(Duration::from_secs(125)), "2m05s");
    }

    #[test]
    fn osc_scanner_parses_133_lifecycle() {
        let mut osc = OscScanner::default();
        let mut out = Vec::new();
        // A prompt + command-output cycle...
        osc.feed(b"\x1b]133;A\x07prompt\x1b]133;C\x07", &mut out);
        osc.feed(b"output\x1b]133;D;0\x07", &mut out);
        // ...plus a marker split across two feeds (the cross-chunk case).
        osc.feed(b"\x1b]133;", &mut out);
        osc.feed(b"B\x07", &mut out);
        let kinds: Vec<_> = out
            .iter()
            .map(|e| match e {
                OscEvent::PromptStart => "A",
                OscEvent::CmdStart => "B",
                OscEvent::OutputStart => "C",
                OscEvent::CmdEnd(Some(0)) => "D0",
                OscEvent::CmdEnd(_) => "D?",
            })
            .collect();
        assert_eq!(kinds, ["A", "C", "D0", "B"]);
    }

    #[test]
    fn scan_alt_screen_tracks_enter_and_exit() {
        assert!(scan_alt_screen(b"\x1b[?1049h", false));
        assert!(!scan_alt_screen(b"\x1b[?1049l", true));
        // Sticky when no toggle is present.
        assert!(scan_alt_screen(b"hello world", true));
        assert!(!scan_alt_screen(b"hello world", false));
        // Legacy 47 / 1047 forms.
        assert!(scan_alt_screen(b"x\x1b[?47hx", false));
        // Enter-then-exit in one chunk → exit wins (later position).
        assert!(!scan_alt_screen(b"\x1b[?1049h...\x1b[?1049l", false));
    }

    #[test]
    fn last_index_of_finds_the_last_match() {
        assert_eq!(last_index_of(b"abcabc", b"abc"), Some(3));
        assert_eq!(last_index_of(b"abc", b"xyz"), None);
        assert_eq!(last_index_of(b"ab", b"abc"), None);
        assert_eq!(last_index_of(b"aaa", b"a"), Some(2));
    }

    #[test]
    fn scroll_region_seq_is_cursor_neutral() {
        // DECSTBM homes the cursor, so the region change must be wrapped
        // in DECSC/DECRC — otherwise the shell's prompt lands on the
        // reserved row after an alt-screen app exits (the btop-quit bug,
        // chevron-dw5.2). This test bites a bare "\x1b[2;Nr".
        let seq = scroll_region_seq(40);
        assert!(seq.starts_with("\x1b7"), "saves the cursor first (DECSC)");
        assert!(seq.ends_with("\x1b8"), "restores the cursor last (DECRC)");
        assert!(seq.contains("\x1b[2;40r"), "sets the region to rows 2..=40");
        assert!(scroll_region_seq(1).is_empty(), "no region when rows < 2");
    }
}
