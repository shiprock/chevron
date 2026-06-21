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

use alacritty_terminal::event::{Event as AcEvent, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as AcConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

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
        let rows = winsz.map_or(24, |w| w.ws_row);
        set_scroll_region(rows);
        // Drop the shell below the bar so its first prompt lands at row 2;
        // host_io_loop paints the bar itself once it starts.
        let _ = write_all(libc::STDOUT_FILENO, b"\x1b[2;1H");
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
        // Ensure the inner shell resolves the SAME chevron binary as this
        // host, so its init (OSC 7 cwd, OSC 133) matches what we parse. A
        // no-op when an installed chevron is already first on PATH.
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
        {
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", dir.display(), path));
        }
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
// A poll-dispatch loop reads clearest in one place (cf. `main`'s dispatch,
// which takes the same allow).
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
    // The bar carries cwd + git (OSC 7) and command state (OSC 133). It is
    // built even when !status — construction is cheap (a cwd read, no git)
    // and it is only refreshed/drawn on the status path. The OSC scanner is
    // Stage 2's backbone.
    let mut osc = OscScanner::default();
    let mut events: Vec<OscEvent> = Vec::new();
    let mut bar = Bar::new(cols, host.to_string());
    if status {
        bar.refresh_git();
        bar.draw();
    }
    // Stage 2 Inc 1: a parallel grid model of the child's screen (only on the
    // status path). Observational — its cursor/alt-screen are surfaced in the
    // bar's debug overlay (CHEVRON_HOST_DEBUG) to prove the model tracks reality.
    let debug = std::env::var_os("CHEVRON_HOST_DEBUG").is_some();
    let mut grid = status.then(|| Grid::new(cols, rows));
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
                bar.cols = cols;
                if let Some(g) = grid.as_mut() {
                    g.resize(cols, rows);
                }
                if status && !alt {
                    set_scroll_region(rows);
                    bar.draw();
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
                if let Some(g) = grid.as_mut() {
                    g.feed(chunk);
                    if debug {
                        let (r, c) = g.cursor();
                        bar.debug = if g.alt_screen() {
                            format!("grid {r}:{c} ALT")
                        } else {
                            format!("grid {r}:{c}")
                        };
                    }
                }
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
                            bar.draw();
                            last_draw = Instant::now();
                        }
                    }
                    // OSC 7 → cwd/git, OSC 133 → command state.
                    osc.feed(chunk, &mut events);
                    let mut changed = false;
                    for ev in events.drain(..) {
                        changed |= bar.apply(ev);
                    }
                    if changed && !alt {
                        bar.draw();
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
            bar.draw();
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

/// The status-bar model: geometry, hostname, the inner shell's working
/// directory + git summary (from OSC 7), and command state (from OSC 133).
struct Bar {
    cols: u16,
    host: String,
    cwd_real: String,
    cwd_disp: String,
    git: String,
    running: bool,
    last_exit: Option<i32>,
    cmd_start: Option<Instant>,
    last_dur: Option<Duration>,
    /// Optional debug overlay (grid cursor + alt flag), Inc 1 observability.
    debug: String,
}

impl Bar {
    /// Cheap to build: reads the process cwd (which equals the inner
    /// shell's cwd at spawn) but computes no git — call [`Bar::refresh_git`].
    fn new(cols: u16, host: String) -> Self {
        let cwd_real = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let cwd_disp = tilde(&cwd_real);
        Self {
            cols,
            host,
            cwd_real,
            cwd_disp,
            git: String::new(),
            running: false,
            last_exit: None,
            cmd_start: None,
            last_dur: None,
            debug: String::new(),
        }
    }

    fn refresh_git(&mut self) {
        self.git = git_summary(&self.cwd_real);
    }

    /// Adopt a new working directory (from OSC 7); recompute git if it
    /// actually changed. Returns whether anything changed.
    fn set_cwd(&mut self, path: String) -> bool {
        if path == self.cwd_real {
            return false;
        }
        self.cwd_disp = tilde(&path);
        self.cwd_real = path;
        self.refresh_git();
        true
    }

    /// Apply an OSC event; return whether the displayed state changed.
    fn apply(&mut self, ev: OscEvent) -> bool {
        match ev {
            OscEvent::OutputStart => {
                self.running = true;
                self.cmd_start = Some(Instant::now());
                true
            }
            OscEvent::CmdEnd(code) => {
                self.running = false;
                self.last_exit = code;
                self.last_dur = self.cmd_start.take().map(|s| s.elapsed());
                // The command may have changed the tree (commit/edit/checkout).
                self.refresh_git();
                true
            }
            OscEvent::Cwd(path) => self.set_cwd(path),
            OscEvent::PromptStart | OscEvent::CmdStart => false,
        }
    }

    fn dur(&self) -> Option<Duration> {
        if self.running {
            self.cmd_start.map(|s| s.elapsed())
        } else {
            self.last_dur
        }
    }

    /// Paint the reserved row: save cursor (DECSC), home, reverse video,
    /// the padded line, reset attrs, restore cursor (DECRC). Save/restore
    /// keeps the child's cursor untouched; row 1 is outside the scroll
    /// region, so this never scrolls the shell's content.
    fn draw(&self) {
        if self.cols == 0 {
            return;
        }
        let status = cmd_status_text(self.running, self.last_exit, self.dur());
        let line = bar_line(
            self.cols,
            now_secs(),
            &self.cwd_disp,
            &self.git,
            &status,
            &self.host,
            &self.debug,
        );
        let seq = format!("\x1b7\x1b[1;1H\x1b[7m{line}\x1b[0m\x1b8");
        let _ = write_all(libc::STDOUT_FILENO, seq.as_bytes());
    }
}

/// Compose the bar text and pad/truncate to exactly `cols` cells. Content
/// is width-1 characters, so cell width equals `chars().count()`.
fn bar_line(
    cols: u16,
    secs: u64,
    cwd: &str,
    git: &str,
    status: &str,
    host: &str,
    debug: &str,
) -> String {
    let width = usize::from(cols);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let place = if git.is_empty() {
        cwd.to_string()
    } else {
        format!("{cwd} {git}")
    };
    let dbg = if debug.is_empty() {
        String::new()
    } else {
        format!("   {debug}")
    };
    let label = format!(" chevron host   {place}   {status}   {host}   {h:02}:{m:02}:{s:02}{dbg} ");
    let mut text: String = label.chars().take(width).collect();
    let len = text.chars().count();
    if len < width {
        text.push_str(&" ".repeat(width - len));
    }
    text
}

/// Replace a leading `$HOME` with `~`.
fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => tilde_with(path, &home),
        _ => path.to_string(),
    }
}

/// Pure core of [`tilde`]: collapse `home` to `~`, matching only on a path
/// boundary so `/Users/mimosa` is not treated as under `/Users/mim`.
fn tilde_with(path: &str, home: &str) -> String {
    if path == home {
        return "~".to_string();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Branch + dirty marker for the repo containing `path`, or empty if it is
/// not a git repo. Uses libgit2 directly (no daemon); only called on
/// cwd-change and command-end, so its cost stays off the byte-forwarding
/// hot path.
fn git_summary(path: &str) -> String {
    let Ok(mut repo) = git2::Repository::discover(path) else {
        return String::new();
    };
    let st = crate::segments::git::RepoStatus::compute(&mut repo);
    if st.is_dirty() {
        format!("{}*", st.branch)
    } else {
        st.branch
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    /// OSC 7 working-directory report (the absolute path).
    Cwd(String),
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
                        if let Some(ev) = parse_osc(&self.buf) {
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

/// Parse an OSC payload (bytes between `ESC]` and the terminator). We care
/// about OSC 133 semantic-prompt markers (`133;A|B|C` / `133;D[;<exit>]`)
/// and OSC 7 (`7;file://<host><path>` — the working directory).
fn parse_osc(buf: &[u8]) -> Option<OscEvent> {
    let s = std::str::from_utf8(buf).ok()?;
    if let Some(rest) = s.strip_prefix("133;") {
        return match rest {
            "A" => Some(OscEvent::PromptStart),
            "B" => Some(OscEvent::CmdStart),
            "C" => Some(OscEvent::OutputStart),
            _ => rest
                .strip_prefix('D')
                .map(|tail| OscEvent::CmdEnd(tail.strip_prefix(';').and_then(|c| c.parse().ok()))),
        };
    }
    if let Some(url) = s.strip_prefix("7;").and_then(|r| r.strip_prefix("file://")) {
        // Skip the host part; the absolute path starts at the first '/'.
        let idx = url.find('/')?;
        return Some(OscEvent::Cwd(url[idx..].to_string()));
    }
    None
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

// ── Stage 2 Inc 1: parallel grid model (alacritty_terminal) ──────────────────
//
// A VT screen model of the CHILD's output, fed the forwarded stream. Inc 1
// uses it observationally: the cursor it tracks (and alt-screen mode) is the
// foundation that retires the shell.rs DSR machinery — chevron reads the
// cursor from this grid instead of querying the terminal with ESC[6n. The
// OscScanner stays for OSC 7/133 (Term does not surface those cleanly).

/// No-op event sink for the embedded Term (we only read its grid).
struct Sink;
impl EventListener for Sink {
    fn send_event(&self, _event: AcEvent) {}
}

/// Window size the embedded Term renders into. No scrollback for the live
/// model (`total_lines == screen_lines`).
#[derive(Clone, Copy)]
struct GridSize {
    cols: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn columns(&self) -> usize {
        self.cols
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn total_lines(&self) -> usize {
        self.lines
    }
}

struct Grid {
    term: Term<Sink>,
    parser: Processor,
}

impl Grid {
    fn new(cols: u16, rows: u16) -> Self {
        let size = GridSize {
            cols: cols as usize,
            lines: rows as usize,
        };
        Self {
            term: Term::new(AcConfig::default(), &size, Sink),
            parser: Processor::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize {
            cols: cols as usize,
            lines: rows as usize,
        });
    }

    /// Cursor as 0-based (row, col).
    fn cursor(&self) -> (usize, usize) {
        let p = self.term.grid().cursor.point;
        (usize::try_from(p.line.0.max(0)).unwrap_or(0), p.column.0)
    }

    fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
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
    fn grid_tracks_cursor_and_alt_screen() {
        let mut g = Grid::new(80, 24);
        g.feed(b"hello");
        assert_eq!(g.cursor(), (0, 5), "5 printed cells -> col 5, row 0");
        g.feed(b"\r\n");
        assert_eq!(g.cursor(), (1, 0), "CR+LF -> row 1, col 0");
        assert!(!g.alt_screen());
        g.feed(b"\x1b[?1049h");
        assert!(g.alt_screen(), "enter alternate screen");
        g.feed(b"\x1b[?1049l");
        assert!(!g.alt_screen(), "leave alternate screen");
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
            let text = bar_line(
                cols,
                45_296,
                "~/src/chevron",
                "main*",
                "ok 1.2s",
                "m-217",
                "",
            );
            assert_eq!(text.chars().count(), usize::from(cols), "width {cols}");
        }
    }

    #[test]
    fn bar_line_renders_cwd_git_status_clock_and_host() {
        // 45_296 s = 12:34:56 UTC.
        let text = bar_line(
            160,
            45_296,
            "~/src/chevron",
            "main*",
            "running 3.4s",
            "m-217",
            "grid 3:5",
        );
        assert!(text.contains("12:34:56"), "got: {text}");
        assert!(text.contains("~/src/chevron"));
        assert!(text.contains("main*"));
        assert!(text.contains("running 3.4s"));
        assert!(text.contains("m-217"));
        assert!(text.contains("grid 3:5"), "debug overlay should appear");
    }

    #[test]
    fn tilde_with_collapses_home_on_a_boundary() {
        assert_eq!(tilde_with("/Users/mim", "/Users/mim"), "~");
        assert_eq!(
            tilde_with("/Users/mim/src/chevron", "/Users/mim"),
            "~/src/chevron"
        );
        // mimosa is NOT under mim — must not collapse.
        assert_eq!(
            tilde_with("/Users/mimosa/x", "/Users/mim"),
            "/Users/mimosa/x"
        );
        assert_eq!(tilde_with("/etc/hosts", "/Users/mim"), "/etc/hosts");
    }

    #[test]
    fn osc_scanner_parses_osc7_cwd() {
        let mut osc = OscScanner::default();
        let mut out = Vec::new();
        osc.feed(b"\x1b]7;file://m-217/Users/mim/src\x07", &mut out);
        match out.as_slice() {
            [OscEvent::Cwd(p)] => assert_eq!(p.as_str(), "/Users/mim/src"),
            other => panic!("expected one Cwd event, got {}", other.len()),
        }
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
                OscEvent::Cwd(_) => "cwd",
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
