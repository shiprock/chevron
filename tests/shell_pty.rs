//! End-to-end PTY tests for the zsh integration (`chevron init zsh`).
//!
//! Each test spawns a real interactive zsh inside a pseudo-terminal and
//! plays the terminal's role: everything the shell emits is fed into a
//! vt100 screen model, and DSR cursor-position queries (`ESC [ 6 n`) are
//! answered the way a physical terminal would — immediately, after a
//! configurable latency (tmux / SSH), or never (terminals without DSR).
//!
//! Assertions run against the *rendered screen grid*, not the byte
//! stream. Transient-rewrite bugs — duplicated chevrons, rewrites landing
//! on the wrong row, DSR responses leaking into the line editor as
//! typeahead — are invisible at the string level; they only exist as
//! wrong screen state.
//!
//! Requires `zsh` on PATH and a UTF-8 locale (both present on macOS and
//! every mainstream Linux distro).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: u16 = 24;
const COLS: u16 = 120;
/// The chevron glyph used by the transient prompt collapse/rewrite.
const CHEVRON: char = '\u{276f}'; // ❯
/// Tail of the live (un-collapsed) prompt: the `$` prompt char followed
/// by the closing powerline arrow. Collapsed lines never contain it.
const LIVE_PROMPT_MARK: &str = "$ \u{e0b0}";
/// The cursor-position query the shell init sends in preexec/precmd.
const DSR_QUERY: &[u8] = b"\x1b[6n";

/// How the emulated terminal answers DSR cursor-position queries.
#[derive(Clone, Copy)]
enum Dsr {
    /// Respond as fast as a local terminal emulator does.
    Immediate,
    /// Respond after a fixed delay, like tmux over a slow SSH link.
    /// Values above the init script's 300 ms `read -t` budget force the
    /// query-timeout path.
    Delayed(Duration),
    /// Never respond, like a terminal that doesn't implement DSR.
    Silent,
    /// Deliver the response one byte at a time with a gap between
    /// bytes, like a congested SSH link fragmenting packets.
    Fragmented(Duration),
    /// Prefix the response with a focus-in report (`ESC [ I`), like a
    /// terminal with focus events enabled when the user clicks into it
    /// mid-exchange.
    FocusNoise,
    /// Answer every query twice, like buggy emulators and some
    /// multiplexer/emulator stacks that double-report. The duplicate
    /// lingers in the input queue to poison the next exchange.
    DoubleResponse,
    /// Answer odd-numbered queries (preexec's) immediately and
    /// even-numbered ones (precmd's rewrite) after a delay past the
    /// 300 ms read budget — an emulator whose load spikes mid-cycle.
    /// Exercises the precmd-side straggler, which has no sweep point
    /// behind it before ZLE takes the terminal.
    AlternateDelayed(Duration),
}

/// Prompt-render mode for the spawned shell.
#[derive(Clone, Copy)]
enum Render {
    /// `CHEVRON_ASYNC` unset: every prompt renders synchronously.
    Sync,
    /// Synchronous, plus a PATH wrapper delaying every render — holds
    /// the post-precmd window (echo restored, ZLE not yet up) open so
    /// straggler-arrival races become deterministic.
    SyncDelayed(Duration),
    /// `CHEVRON_ASYNC=1`: cached prompt plus background refresh.
    Async,
    /// `CHEVRON_ASYNC=1` plus a PATH wrapper delaying every render —
    /// the lever that makes refresh-vs-next-cycle races deterministic.
    AsyncDelayed(Duration),
}

struct Term {
    parser: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // Also keeps the master side of the PTY open for the reader thread.
    master: Box<dyn portable_pty::MasterPty>,
    _home: tempfile::TempDir,
}

// ── screen helpers (free functions so wait_for predicates can use them) ─────

fn lines(screen: &vt100::Screen) -> Vec<String> {
    // Read the width off the screen, not the spawn-time constant — tests
    // may have resized the terminal.
    let (_, cols) = screen.size();
    screen
        .rows(0, cols)
        .map(|r| r.trim_end().to_string())
        .collect()
}

/// Rows that contain at least one chevron glyph. After N collapsed
/// commands there must be exactly N of these — one per command.
fn chevron_rows(screen: &vt100::Screen) -> Vec<String> {
    lines(screen)
        .into_iter()
        .filter(|l| l.contains(CHEVRON))
        .collect()
}

/// Total chevron glyphs on screen. Catches duplicates that land on the
/// same row (`❯ ❯ true`), which a row count alone would miss.
fn chevron_glyphs(screen: &vt100::Screen) -> usize {
    lines(screen)
        .iter()
        .map(|l| l.matches(CHEVRON).count())
        .sum()
}

fn last_nonempty(screen: &vt100::Screen) -> String {
    lines(screen)
        .into_iter()
        .rev()
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

/// The live prompt is painted and is the bottom-most content on screen.
fn prompt_ready(screen: &vt100::Screen) -> bool {
    last_nonempty(screen).contains(LIVE_PROMPT_MARK)
}

/// Longest proper prefix of `DSR_QUERY` that is a suffix of `pend` —
/// bytes we must hold back in case the query is split across reads.
fn query_prefix_holdback(pend: &[u8]) -> usize {
    (1..DSR_QUERY.len())
        .rev()
        .find(|&keep| pend.len() >= keep && pend[pend.len() - keep..] == DSR_QUERY[..keep])
        .unwrap_or(0)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// These are integration tests of the *zsh* init; environments without
/// zsh (the nix build sandbox, minimal CI images) skip them — loudly —
/// instead of failing at PTY spawn.
fn zsh_available() -> bool {
    let ok = std::process::Command::new("zsh")
        .args(["-fc", "exit 0"])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipping PTY test: zsh not available on PATH");
    }
    ok
}

/// Reader-thread body: feeds shell output into the vt100 screen and
/// answers DSR queries with the cursor position *at the moment the query
/// arrives* — exactly what a real terminal reports — even when delivery
/// of the response is delayed.
fn pump_output(
    reader: &mut dyn Read,
    parser: &Mutex<vt100::Parser>,
    raw: &Mutex<Vec<u8>>,
    dsr: Dsr,
    tx: &mpsc::Sender<(Instant, Vec<u8>)>,
) {
    let mut buf = [0u8; 8192];
    let mut pend: Vec<u8> = Vec::new();
    let mut query_no: usize = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        raw.lock().unwrap().extend_from_slice(&buf[..n]);
        pend.extend_from_slice(&buf[..n]);
        loop {
            if let Some(i) = find_subslice(&pend, DSR_QUERY) {
                let end = i + DSR_QUERY.len();
                let mut p = parser.lock().unwrap();
                p.process(&pend[..end]);
                let (row, col) = p.screen().cursor_position();
                drop(p);
                pend.drain(..end);
                query_no += 1;
                let resp = format!("\x1b[{};{}R", row + 1, col + 1).into_bytes();
                match dsr {
                    Dsr::Silent => {}
                    Dsr::Immediate => {
                        let _ = tx.send((Instant::now(), resp));
                    }
                    Dsr::Delayed(d) => {
                        let _ = tx.send((Instant::now() + d, resp));
                    }
                    Dsr::Fragmented(step) => {
                        let mut due = Instant::now();
                        for b in resp {
                            due += step;
                            if tx.send((due, vec![b])).is_err() {
                                break;
                            }
                        }
                    }
                    Dsr::FocusNoise => {
                        let mut noisy = b"\x1b[I".to_vec();
                        noisy.extend_from_slice(&resp);
                        let _ = tx.send((Instant::now(), noisy));
                    }
                    Dsr::DoubleResponse => {
                        let _ = tx.send((Instant::now(), resp.clone()));
                        let _ = tx.send((Instant::now(), resp));
                    }
                    Dsr::AlternateDelayed(d) => {
                        let due = if query_no % 2 == 1 {
                            Instant::now()
                        } else {
                            Instant::now() + d
                        };
                        let _ = tx.send((due, resp));
                    }
                }
            } else {
                let keep = query_prefix_holdback(&pend);
                let feed = pend.len() - keep;
                if feed > 0 {
                    parser.lock().unwrap().process(&pend[..feed]);
                    pend.drain(..feed);
                }
                break;
            }
        }
    }
}

impl Term {
    /// Spawn `zsh -i` in a fresh PTY with a hermetic `$HOME` whose
    /// `.zshrc` is exactly the documented install line. The test-built
    /// chevron binary is first in `path`.
    fn spawn_zsh(dsr: Dsr) -> Self {
        Self::spawn_inner(dsr, ROWS, COLS, Render::Sync)
    }

    fn spawn_zsh_sized(dsr: Dsr, rows: u16, cols: u16) -> Self {
        Self::spawn_inner(dsr, rows, cols, Render::Sync)
    }

    /// Spawn with `CHEVRON_ASYNC=1`. `render_delay` installs a PATH
    /// wrapper that slows every `chevron prompt` render — the lever
    /// that makes background-refresh-vs-next-cycle races deterministic.
    fn spawn_zsh_async(dsr: Dsr, render_delay: Option<Duration>) -> Self {
        let render = match render_delay {
            Some(d) => Render::AsyncDelayed(d),
            None => Render::Async,
        };
        Self::spawn_inner(dsr, ROWS, COLS, render)
    }

    fn spawn_inner(dsr: Dsr, rows: u16, cols: u16, mode: Render) -> Self {
        let home = tempfile::TempDir::new().unwrap();
        // macOS tempdirs live behind a /var -> /private/var symlink;
        // canonicalize so $HOME matches what getcwd() reports and the
        // path segment renders as `~`.
        let home_path = home.path().canonicalize().unwrap();
        let bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_chevron"))
            .parent()
            .unwrap()
            .to_path_buf();

        // Skip /etc/zshrc & friends — only our fixture configures the
        // shell. (/etc/zshenv still runs; NO_RCS can't disable it.)
        std::fs::write(home_path.join(".zshenv"), "setopt no_global_rcs\n").unwrap();
        // Async render-delay wrapper: shadows the real binary on PATH
        // and sleeps before `prompt` renders, so a background refresh
        // reliably outlives the next prompt cycle.
        let mut path_dirs = bin_dir.display().to_string();
        if let Render::AsyncDelayed(delay) | Render::SyncDelayed(delay) = mode {
            use std::os::unix::fs::PermissionsExt;
            let wrapper_dir = home_path.join("wrapper-bin");
            std::fs::create_dir_all(&wrapper_dir).unwrap();
            let wrapper = wrapper_dir.join("chevron");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\n[ \"$1\" = prompt ] && sleep {}\nexec {}/chevron \"$@\"\n",
                    delay.as_secs_f64(),
                    bin_dir.display()
                ),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&wrapper).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&wrapper, perms).unwrap();
            path_dirs = format!("{} {}", wrapper_dir.display(), path_dirs);
        }
        // Prepend path *in .zshrc* so it wins over anything /etc/zshenv
        // (nix-darwin, etc.) prepends after CommandBuilder's env.
        std::fs::write(
            home_path.join(".zshrc"),
            format!("path=({path_dirs} $path)\neval \"$(chevron init zsh)\"\n"),
        )
        .unwrap();

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        let mut cmd = CommandBuilder::new("zsh");
        cmd.arg("-i");
        cmd.cwd(&home_path);
        cmd.env("HOME", &home_path);
        cmd.env("TERM", "xterm-256color");
        cmd.env("LANG", "en_US.UTF-8");
        cmd.env("LC_ALL", "en_US.UTF-8");
        cmd.env("CHEVRON_NO_DAEMON", "1");
        // The async prompt cache lives under XDG_RUNTIME_DIR; point it
        // at the test home so parallel tests (and the developer's real
        // shells) never share cache state.
        cmd.env("XDG_RUNTIME_DIR", &home_path);
        // The test shell must not talk to an enclosing tmux server
        // (rename-window would hit the developer's session) nor inherit
        // chevron knobs from the developer's environment.
        for var in [
            "TMUX",
            "TMUX_PANE",
            "ZDOTDIR",
            "CHEVRON_TRANSIENT",
            "CHEVRON_OSC133",
            "CHEVRON_ASYNC",
            "CHEVRON_CACHE_FILE",
            "CHEVRON_TRANSIENT_DURATION_MS",
            "CHEVRON_HISTORY",
        ] {
            cmd.env_remove(var);
        }
        if matches!(mode, Render::Async | Render::AsyncDelayed(_)) {
            cmd.env("CHEVRON_ASYNC", "1");
        }

        let child = pair.slave.spawn_command(cmd).unwrap();
        // Close our copy of the slave so master reads EOF once zsh exits.
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 200)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let mut reader = pair.master.try_clone_reader().unwrap();

        // Responder thread: delivers DSR responses at their due time, in
        // FIFO order, through the same writer the tests type through.
        let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
        let responder_writer = Arc::clone(&writer);
        std::thread::spawn(move || {
            while let Ok((due, bytes)) = rx.recv() {
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
                let mut w = responder_writer.lock().unwrap();
                let _ = w.write_all(&bytes);
                let _ = w.flush();
            }
        });

        let reader_parser = Arc::clone(&parser);
        let reader_raw = Arc::clone(&raw);
        std::thread::spawn(move || pump_output(&mut reader, &reader_parser, &reader_raw, dsr, &tx));

        let term = Term {
            parser,
            raw,
            writer,
            child,
            master: pair.master,
            _home: home,
        };
        term.wait_for("initial prompt", prompt_ready);
        term
    }

    fn send(&self, s: &str) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(s.as_bytes()).unwrap();
        w.flush().unwrap();
    }

    /// Type a command and press Enter (terminals send CR for Enter).
    fn send_line(&self, s: &str) {
        self.send(&format!("{s}\r"));
    }

    /// Resize the terminal the way a real emulator does: shrink/grow the
    /// rendered grid, then update the kernel winsize (which raises
    /// SIGWINCH in the foreground process group).
    fn resize(&self, rows: u16, cols: u16) {
        self.parser
            .lock()
            .unwrap()
            .screen_mut()
            .set_size(rows, cols);
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    }

    /// Send a command line, then wait until it has collapsed to a `❯`
    /// row and the next live prompt is painted below it.
    fn run(&self, cmd_line: &str) {
        let before = self.with_screen(|s| chevron_rows(s).len());
        self.send_line(cmd_line);
        self.wait_for(
            &format!("`{cmd_line}` to collapse and reprompt"),
            move |s| chevron_rows(s).len() > before && prompt_ready(s),
        );
    }

    fn with_screen<T>(&self, f: impl FnOnce(&vt100::Screen) -> T) -> T {
        let p = self.parser.lock().unwrap();
        f(p.screen())
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&vt100::Screen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.with_screen(&pred) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n{}",
                self.dump()
            );
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Wait until the shell has emitted nothing for `quiet` — used after
    /// latency tests so straggling DSR responses land before asserting.
    fn wait_settled(&self, quiet: Duration) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_len = self.raw.lock().unwrap().len();
        let mut last_change = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(15));
            let len = self.raw.lock().unwrap().len();
            if len == last_len {
                if last_change.elapsed() >= quiet {
                    return;
                }
            } else {
                last_len = len;
                last_change = Instant::now();
            }
            assert!(
                Instant::now() < deadline,
                "output never settled\n{}",
                self.dump()
            );
        }
    }

    fn chevron_cell_color(&self, row: usize, line: &str) -> vt100::Color {
        let col = line.chars().position(|c| c == CHEVRON).unwrap();
        self.with_screen(|s| {
            s.cell(u16::try_from(row).unwrap(), u16::try_from(col).unwrap())
                .unwrap()
                .fgcolor()
        })
    }

    /// Foreground color of the chevron glyph on the given row.
    fn chevron_color_on_row(&self, row_text: &str) -> vt100::Color {
        let (row, line) = self
            .with_screen(lines)
            .into_iter()
            .enumerate()
            .find(|(_, l)| l == row_text)
            .unwrap_or_else(|| panic!("row {row_text:?} not on screen\n{}", self.dump()));
        self.chevron_cell_color(row, &line)
    }

    /// Foreground color of the chevron on the n-th (0-based) chevron row
    /// — for screens where several collapsed rows have identical text.
    fn chevron_color_on_nth(&self, n: usize) -> vt100::Color {
        let (row, line) = self
            .with_screen(lines)
            .into_iter()
            .enumerate()
            .filter(|(_, l)| l.contains(CHEVRON))
            .nth(n)
            .unwrap_or_else(|| panic!("fewer than {} chevron rows\n{}", n + 1, self.dump()));
        self.chevron_cell_color(row, &line)
    }

    /// Screen + escaped raw-byte tail, for failure messages. This is the
    /// place to look when a test fails: the grid shows *what* went wrong,
    /// the raw tail shows *which bytes* caused it.
    fn dump(&self) -> String {
        let grid = self
            .with_screen(lines)
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{i:2} |{l}"))
            .collect::<Vec<_>>()
            .join("\n");
        let raw = self.raw.lock().unwrap();
        let tail_start = raw.len().saturating_sub(600);
        let tail: String = String::from_utf8_lossy(&raw[tail_start..])
            .chars()
            .map(|c| if c == '\x1b' { '\u{241b}' } else { c }) // ␛
            .collect();
        format!("── screen ──\n{grid}\n── raw tail (\u{241b} = ESC) ──\n{tail}\n")
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const GREEN: vt100::Color = vt100::Color::Idx(2);
const RED: vt100::Color = vt100::Color::Idx(1);

// ── the invariant ────────────────────────────────────────────────────────────
// After running N commands, exactly N rows contain a chevron (one collapsed
// line per command), with exactly one glyph each. The live prompt never
// contains one. Every test below is some instantiation of this.

#[test]
fn builtin_collapses_to_single_chevron_line() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(rows, vec!["\u{276f} true"], "{}", t.dump());
    assert_eq!(t.with_screen(chevron_glyphs), 1, "{}", t.dump());
}

#[test]
fn rewrite_colors_chevron_by_exit_status() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("true");
    t.run("false");
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} true", "\u{276f} false"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "exit 0 chevron should be rewritten green\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} false"),
        RED,
        "exit 1 chevron should be rewritten red\n{}",
        t.dump()
    );
}

#[test]
fn repeated_builtins_collapse_one_row_each() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    for cmd in ["true", ":", "export CHEVRON_TEST_X=1", "cd /"] {
        t.run(cmd);
    }
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec![
            "\u{276f} true",
            "\u{276f} :",
            "\u{276f} export CHEVRON_TEST_X=1",
            "\u{276f} cd /",
        ],
        "{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 4, "{}", t.dump());
}

#[test]
fn external_command_keeps_output_and_single_chevron() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("/bin/echo hello-from-pty");
    t.wait_settled(Duration::from_millis(150));

    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "hello-from-pty"),
        "command output should survive the rewrite\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} /bin/echo hello-from-pty"],
        "{}",
        t.dump()
    );
}

/// Two commands typed faster than the shell can repaint: the second line
/// is sitting in the tty input queue while preexec/precmd run their DSR
/// exchanges for the first. This is the everyday "typed ahead after a
/// builtin" case.
///
/// Regression test: the DSR `read -d 'R'` used to consume the typed-ahead
/// `true\r` together with the cursor response — the second command never
/// executed and reappeared parked in the next edit buffer, and the
/// transcript showed precmd's response kernel-echoed as literal
/// `^[[2;1R`. The query helper now probes for pending input and skips
/// the exchange rather than racing the user's keystrokes: a command run
/// with typeahead behind it may keep a neutral chevron, but every typed
/// command must execute and collapse to exactly one row.
#[test]
fn rapid_consecutive_builtins_no_duplicates() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send("true\rtrue\r");
    t.wait_for("both commands to collapse and reprompt", |s| {
        chevron_rows(s).len() >= 2 && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(200));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} true", "\u{276f} true"],
        "each command must collapse to exactly one chevron row\n{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 2, "{}", t.dump());
    let prompt = t.with_screen(last_nonempty);
    assert!(
        !prompt.contains(CHEVRON) && !prompt.contains('R'),
        "live prompt must not contain leaked chevrons or DSR fragments: {prompt:?}\n{}",
        t.dump()
    );
    // The second command's DSR exchange runs against an empty queue, so
    // its rewrite must still happen.
    assert_eq!(
        t.chevron_color_on_nth(1),
        GREEN,
        "second command's chevron should be color-corrected\n{}",
        t.dump()
    );
}

/// `echo` is the interesting builtin: unlike `true` it produces output,
/// so the cursor row moves between preexec's query and precmd's — the
/// rewrite math runs with a non-zero delta.
#[test]
fn builtin_echo_collapses_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("echo hi");
    t.wait_settled(Duration::from_millis(150));

    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "hi"),
        "echo output must survive the rewrite\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} echo hi"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} echo hi"),
        GREEN,
        "{}",
        t.dump()
    );
}

/// Output without a trailing newline leaves the cursor mid-row: the
/// precmd query and the rewrite's reposition must put the cursor back
/// where the command left it — row AND column — or zsh's partial-line
/// handling (the inverse `%` mark) paints over the output.
#[test]
fn builtin_echo_no_newline_keeps_partial_output() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("echo -n hi");
    t.wait_settled(Duration::from_millis(150));

    let all = t.with_screen(lines);
    assert!(
        // zsh appends its PROMPT_EOL_MARK (`%`) right after the partial
        // output, so the row reads `hi%`.
        all.iter().any(|l| l.starts_with("hi")),
        "partial-line output must not be stomped by the reposition\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} echo -n hi"],
        "{}",
        t.dump()
    );
}

/// Empty enters: accept-line collapses the prompt to a bare chevron but
/// preexec never fires (there is no command), so no DSR state must leak
/// into the next real command's rewrite.
#[test]
fn empty_enters_leave_bare_chevrons_and_no_stale_state() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send("\r");
    t.wait_for("first bare chevron", |s| {
        chevron_rows(s).len() == 1 && prompt_ready(s)
    });
    t.send("\r");
    t.wait_for("second bare chevron", |s| {
        chevron_rows(s).len() == 2 && prompt_ready(s)
    });
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f}", "\u{276f}", "\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(2),
        GREEN,
        "the real command after empty enters must still be rewritten\n{}",
        t.dump()
    );
}

/// `clear` moves the cursor ABOVE the saved row (negative delta): the
/// rewrite must be skipped, leaving a clean screen with just the prompt.
#[test]
fn clear_builtin_skips_rewrite_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("true");
    t.send_line("clear");
    t.wait_for("screen cleared back to a lone prompt", |s| {
        chevron_rows(s).is_empty() && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(150));

    let all = t.with_screen(lines);
    assert!(
        all[0].contains(LIVE_PROMPT_MARK),
        "prompt must be at the top after clear\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_glyphs),
        0,
        "no rewrite may resurrect a chevron after clear\n{}",
        t.dump()
    );
}

/// Output that scrolls the saved row off-screen: the scroll guards must
/// skip the rewrite entirely — a rewrite here would paint a chevron into
/// the middle of the output.
#[test]
fn scrolling_output_skips_rewrite() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // Not run(): the collapsed row scrolls off-screen, so waiting for a
    // chevron row would never succeed. Wait on the output tail instead.
    t.send_line("print -l {1..40}");
    t.wait_for("output to finish and reprompt", |s| {
        lines(s).iter().any(|l| l == "40") && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(150));

    let all = t.with_screen(lines);
    // The collapsed line scrolled off; every chevron-free row is either
    // a number from the output or blank, and no chevron was painted into
    // the output.
    assert_eq!(
        t.with_screen(chevron_glyphs),
        0,
        "rewrite must not paint into scrolled output\n{}",
        t.dump()
    );
    assert!(
        all.iter().any(|l| l == "40"),
        "output tail must be visible\n{}",
        t.dump()
    );
}

/// A command issued from the bottom row of the screen: the collapse
/// itself scrolls, so `R_saved` sits at the screen edge and the guards
/// must leave the chevron neutral rather than rewrite a wrong row.
#[test]
fn command_from_bottom_row_skips_rewrite() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // Park the prompt on the bottom row.
    t.run("print -l {1..21}");
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} print -l {1..21}", "\u{276f} true"],
        "exactly one chevron row per command, no duplicates\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(1),
        vt100::Color::Default,
        "bottom-row command must stay neutral (rewrite skipped)\n{}",
        t.dump()
    );
}

/// Input wider than the terminal wraps the collapsed line across two
/// rows.
///
/// Regression test: the rewrite used to target only R_saved-1 — the
/// wrap REMAINDER row, not the chevron's row — erasing the remainder,
/// re-painting the full command there (wrapping over the next-prompt
/// row), and leaving the original chevron row untouched: two chevron
/// rows on screen. On an 80-column terminal any command past ~78 chars
/// triggered it. The rewrite now measures the wrapped span and erases
/// all of it.
#[test]
fn wrapped_input_line_no_duplicate_chevrons() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // `:` ignores its arguments; `❯ : ` (4 cells) + 148 `x`s = 152
    // cells, wrapping a 120-column screen onto a second row.
    let long = format!(": {}", "x".repeat(148));
    t.run(&long);
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec![format!("\u{276f} : {}", "x".repeat(116))],
        "a wrapped command must collapse to a single chevron row\n{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 1, "{}", t.dump());
    // The wrap remainder re-wraps onto the row below, intact.
    let all = t.with_screen(lines);
    assert_eq!(
        all[1],
        "x".repeat(32),
        "wrap remainder must survive the rewrite\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(0),
        GREEN,
        "wrapped command's chevron must still be color-corrected\n{}",
        t.dump()
    );
    let prompt = t.with_screen(last_nonempty);
    assert!(
        prompt.contains(LIVE_PROMPT_MARK),
        "live prompt must be intact below the wrapped line\n{}",
        t.dump()
    );
}

/// Resize while sitting at the prompt: ZLE handles SIGWINCH itself and
/// repaints; the next command cycle runs entirely under the new
/// geometry, so the rewrite must keep working.
#[test]
fn resize_at_prompt_keeps_rewrite_working() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("true");
    t.resize(24, 80);
    t.wait_settled(Duration::from_millis(300));
    t.run("false");
    t.wait_settled(Duration::from_millis(150));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true", "\u{276f} false"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(1),
        RED,
        "post-resize cycle must still color-correct\n{}",
        t.dump()
    );
}

/// Resize while a command is RUNNING: every coordinate saved in preexec
/// is now meaningless (reflowing terminals rewrap the transient to a
/// different row entirely), and recomputing the wrap span under the new
/// width makes the erase loop eat rows it never painted — here, the
/// previous command's collapsed line. The rewrite must detect the
/// geometry change and skip.
#[test]
fn resize_during_command_skips_rewrite() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // Sentinel: a collapsed row sitting directly above the transient.
    t.run("true");
    // Wide enough to wrap at 120 cols (2 rows) — and at the post-resize
    // 60 cols the naive span recompute says 3 rows, one of which is the
    // sentinel.
    let wide = format!("sleep 0.4 && : {}", "x".repeat(133));
    t.send_line(&wide);
    // Collapse + preexec query finish in milliseconds; the resize lands
    // squarely inside the sleep.
    std::thread::sleep(Duration::from_millis(200));
    t.resize(24, 60);
    t.wait_for("reprompt after mid-command resize", prompt_ready);
    t.wait_settled(Duration::from_millis(200));

    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "\u{276f} true"),
        "rewrite after a resize must not erase rows it never painted\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_glyphs),
        2,
        "one chevron per command, no duplicates\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(1),
        vt100::Color::Default,
        "geometry changed mid-command: rewrite must skip, chevron stays neutral\n{}",
        t.dump()
    );
}

/// A congested link delivering the DSR response one byte at a time
/// (~25 ms apart, well inside the 300 ms budget): the exchange must
/// still complete and the rewrite must happen.
#[test]
fn fragmented_dsr_response_still_rewrites() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Fragmented(Duration::from_millis(25)));
    t.run("true");
    t.wait_settled(Duration::from_millis(400));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "fragmented response must still complete the rewrite\n{}",
        t.dump()
    );
}

/// A focus-in report (`ESC [ I`) lands ahead of the DSR response — a
/// terminal with focus events enabled while the user clicks into it.
/// The parser must take the LAST CSI in the buffer and validate the row
/// is numeric; parsing the first CSI fed `I\e[5` into zsh arithmetic
/// and printed a math error at the prompt.
#[test]
fn focus_event_during_dsr_exchange_is_harmless() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::FocusNoise);
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "the real response follows the noise and must still be used\n{}",
        t.dump()
    );
    let all = t.with_screen(lines);
    assert!(
        !all.iter()
            .any(|l| l.contains("bad math") || l.contains("expression")),
        "no zsh arithmetic errors may leak to the screen\n{}",
        t.dump()
    );
}

/// Wide (CJK) characters count two cells each: the wrap-span math must
/// measure display cells, not chars or bytes, or the erase loop misses
/// rows. `❯ : ` (4) + 70 × あ (140) = 144 cells → 2 rows at 120 cols.
#[test]
fn wide_char_wrapped_input_rewrites_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    let cmd = format!(": {}", "\u{3042}".repeat(70));
    t.run(&cmd);
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec![format!("\u{276f} : {}", "\u{3042}".repeat(58))],
        "wide-char command must collapse to one chevron row\n{}",
        t.dump()
    );
    let all = t.with_screen(lines);
    assert_eq!(
        all[1],
        "\u{3042}".repeat(12),
        "wide-char wrap remainder must survive the rewrite\n{}",
        t.dump()
    );
    assert_eq!(t.chevron_color_on_nth(0), GREEN, "{}", t.dump());
}

/// Ctrl-C on a half-typed line: the accept-line widget never runs, so
/// nothing collapses and no DSR state is saved; the abandoned line must
/// not leave chevrons or confuse the next real command.
#[test]
fn ctrl_c_aborts_typed_line_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send("echo oops");
    t.wait_for("typed text to echo", |s| {
        lines(s).iter().any(|l| l.contains("echo oops"))
    });
    t.send("\x03");
    t.wait_for("fresh prompt after interrupt", prompt_ready);
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "the aborted line must not collapse or duplicate\n{}",
        t.dump()
    );
    assert!(
        !t.with_screen(lines).iter().any(|l| l == "oops"),
        "the aborted command must not run\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "{}",
        t.dump()
    );
}

/// Ctrl-C arriving in the same instant as Enter — the interrupt byte
/// lands inside the preexec DSR exchange. Whatever zsh does with the
/// interrupt, the screen must stay clean: no `^C` re-injected into the
/// next edit buffer, no chevron duplicates, and the shell must remain
/// fully usable.
#[test]
fn ctrl_c_racing_the_dsr_exchange_is_harmless() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send("true\r\x03");
    t.wait_for("prompt after interrupted cycle", prompt_ready);
    t.wait_settled(Duration::from_millis(300));

    assert!(
        t.with_screen(chevron_glyphs) <= 1,
        "at most the collapsed `true` may carry a chevron\n{}",
        t.dump()
    );
    // The shell must still work, with no leftover control bytes in the
    // edit buffer.
    t.run("echo after");
    t.wait_settled(Duration::from_millis(150));
    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "after"),
        "shell must remain usable after the race\n{}",
        t.dump()
    );
    assert!(
        all.iter()
            .all(|l| !l.contains("\u{2403}") && !l.contains("^C\u{276f}")),
        "no control-byte garbage may persist\n{}",
        t.dump()
    );
}

/// A full-screen application (vim, less): enters the alternate screen,
/// draws, and leaves. The cursor is restored on exit, so the rewrite
/// must work exactly as for a no-output builtin — and nothing from the
/// alternate screen may bleed into the main one.
#[test]
fn alt_screen_app_restores_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send_line("printf '\\e[?1049h\\e[2J\\e[HFULLSCREEN'; sleep 0.6; printf '\\e[?1049l'");
    t.wait_for("app to enter the alternate screen", |s| {
        s.alternate_screen() && lines(s).iter().any(|l| l == "FULLSCREEN")
    });
    t.wait_for("app to leave the alternate screen", |s| {
        !s.alternate_screen() && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(200));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows.len(),
        1,
        "one collapsed row, no duplicates\n{}",
        t.dump()
    );
    assert!(
        rows[0].contains("1049h"),
        "collapsed row should show the typed command\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(0),
        GREEN,
        "rewrite must work after an alt-screen round trip\n{}",
        t.dump()
    );
    // The typed command itself contains the word; only rows BESIDES the
    // collapsed command line count as bleed.
    assert!(
        !t.with_screen(lines)
            .iter()
            .filter(|l| !l.contains(CHEVRON))
            .any(|l| l.contains("FULLSCREEN")),
        "alternate-screen content must not bleed into the main screen\n{}",
        t.dump()
    );
}

/// A bracketed paste (terminal wraps pasted text in `ESC[200~ … 201~`)
/// followed by Enter: ZLE strips the markers, the buffer collapses and
/// rewrites like typed input.
#[test]
fn bracketed_paste_collapses_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send("\x1b[200~echo pasted-text\x1b[201~");
    t.wait_for("paste to land in the buffer", |s| {
        lines(s).iter().any(|l| l.contains("echo pasted-text"))
    });
    t.send("\r");
    t.wait_for("pasted command to collapse and run", |s| {
        chevron_rows(s).len() == 1 && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(150));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} echo pasted-text"],
        "{}",
        t.dump()
    );
    assert!(
        t.with_screen(lines).iter().any(|l| l == "pasted-text"),
        "pasted command must execute\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} echo pasted-text"),
        GREEN,
        "{}",
        t.dump()
    );
}

/// A buggy emulator answering every DSR query twice: the duplicate
/// response lingers in the input queue. The pending-input probe must
/// notice it on the next exchange and skip rather than read a stale
/// row — the wrong-row-rewrite class of bug.
#[test]
fn double_responding_terminal_degrades_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::DoubleResponse);
    t.run("true");
    t.wait_settled(Duration::from_millis(300));
    t.run("false");
    t.wait_settled(Duration::from_millis(300));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true", "\u{276f} false"],
        "each command exactly one chevron row, no wrong-row rewrites\n{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 2, "{}", t.dump());
    let prompt = t.with_screen(last_nonempty);
    assert!(
        prompt.contains(LIVE_PROMPT_MARK),
        "stray duplicate responses must not garble the prompt: {prompt:?}\n{}",
        t.dump()
    );
}

/// Ctrl-Z suspend and `fg` resume: two full prompt cycles wrap around a
/// job-control state change; the suspended job's collapsed row must not
/// duplicate or get rewritten onto the job-control message.
#[test]
fn suspend_resume_cycle_keeps_rows_intact() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send_line("sleep 5");
    t.wait_for("command to collapse and start", |s| {
        chevron_rows(s).len() == 1
    });
    // The collapse paints before preexec hands the terminal to the
    // child; ^Z must hit the CHILD's process group. Too early and zsh
    // (which ignores TSTP) eats it; too late is harmless within the 5 s
    // sleep window.
    std::thread::sleep(Duration::from_millis(200));
    t.send("\x1a"); // Ctrl-Z
    t.wait_for("suspend message and prompt", |s| {
        lines(s).iter().any(|l| l.contains("suspended")) && prompt_ready(s)
    });
    t.send_line("fg");
    t.wait_for("job to resume in the foreground", |s| {
        lines(s)
            .iter()
            .any(|l| l.contains("continued") || l.contains("running"))
    });
    t.send("\x03"); // kill the resumed sleep
    t.wait_for("prompt after killing resumed job", prompt_ready);
    t.wait_settled(Duration::from_millis(200));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} sleep 5", "\u{276f} fg"],
        "suspend/resume must leave exactly one row per command\n{}",
        t.dump()
    );
    assert!(
        t.with_screen(lines)
            .iter()
            .any(|l| l.contains("continued") || l.contains("running")),
        "fg should have resumed the job\n{}",
        t.dump()
    );
}

/// A tiny terminal (8 rows): every cycle runs against the bottom edge,
/// so the scroll guards fire constantly. Counts must stay exact and the
/// shell usable — no chevron may ever be painted into scrolled content.
#[test]
fn tiny_terminal_stays_consistent() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh_sized(Dsr::Immediate, 8, 60);
    for cmd in ["true", "false", "echo tiny"] {
        t.run(cmd);
    }
    t.wait_settled(Duration::from_millis(200));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} true", "\u{276f} false", "\u{276f} echo tiny"],
        "{}",
        t.dump()
    );
    assert!(
        t.with_screen(lines).iter().any(|l| l == "tiny"),
        "output must survive at the bottom edge\n{}",
        t.dump()
    );
}

/// Multi-line input through a PS2 continuation (`echo 'a` ⏎ `b'`).
///
/// Regression test: the rewrite measured only the FIRST line of the
/// command, so it erased the continuation row and painted a second
/// `❯ echo 'a` there — a duplicated chevron. Multi-line input now skips
/// the rewrite entirely (neutral chevron, rows left as painted).
#[test]
fn multiline_ps2_input_no_duplicate_chevrons() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send_line("echo 'a");
    t.wait_for("PS2 continuation prompt", |s| {
        lines(s).iter().any(|l| l.contains("quote"))
    });
    t.send_line("b'");
    t.wait_for("command to run and reprompt", |s| {
        lines(s).iter().any(|l| l == "b") && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(200));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(
        rows,
        vec!["\u{276f} echo 'a"],
        "multi-line input must keep exactly one chevron\n{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 1, "{}", t.dump());
    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "a") && all.iter().any(|l| l == "b"),
        "both output lines must be present\n{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_nth(0),
        vt100::Color::Default,
        "multi-line transient stays neutral by design\n{}",
        t.dump()
    );
}

/// The post-exec duration tag: a dim ` N.Ns` row between a slow
/// command's output and the next prompt.
///
/// Regression test: the tag reads `$EPOCHREALTIME`, which expands EMPTY
/// unless zsh/datetime is loaded — and the init never loaded it, so in
/// a bare `.zshrc` every command measured 0 ms and the shipped feature
/// silently never fired.
#[test]
fn duration_tag_renders_for_slow_commands() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // Lower the threshold so the test stays fast; exported in-shell to
    // keep the spawn fixture identical to the documented install.
    t.run("export CHEVRON_TRANSIENT_DURATION_MS=100");
    t.run("sleep 0.2");
    t.wait_settled(Duration::from_millis(200));

    // ` 0.2s`-shaped row (digits may vary with scheduler overshoot).
    let is_tag = |l: &str| {
        l.strip_prefix(' ')
            .and_then(|b| b.strip_suffix('s'))
            .and_then(|b| b.split_once('.'))
            .is_some_and(|(a, b)| {
                !a.is_empty()
                    && a.chars().all(|c| c.is_ascii_digit())
                    && !b.is_empty()
                    && b.chars().all(|c| c.is_ascii_digit())
            })
    };
    let all = t.with_screen(lines);
    let tag_rows: Vec<usize> = all
        .iter()
        .enumerate()
        .filter_map(|(i, l)| is_tag(l).then_some(i))
        .collect();
    // Exactly one: the instant `export` above must NOT be tagged — the
    // measurement brackets the command only, not the DSR machinery
    // around it (which costs tens of ms and used to be billed to the
    // command under a lowered threshold).
    assert_eq!(
        tag_rows.len(),
        1,
        "exactly the slow command gets a duration tag\n{}",
        t.dump()
    );
    let tag_row = tag_rows[0];
    let sleep_row = all.iter().position(|l| l == "\u{276f} sleep 0.2").unwrap();
    assert!(
        tag_row > sleep_row,
        "tag must sit between the command and the prompt\n{}",
        t.dump()
    );
    let dim = t.with_screen(|s| {
        s.cell(u16::try_from(tag_row).unwrap(), 1)
            .is_some_and(vt100::Cell::dim)
    });
    assert!(dim, "duration tag must be dim-styled\n{}", t.dump());
}

// ── async fast path (CHEVRON_ASYNC=1) ────────────────────────────────────────

/// Async basics: the first prompt is a sync cache miss; later cycles
/// serve the cached prompt instantly and repaint from the background
/// refresh. Collapse and rewrite must behave exactly as in sync mode.
#[test]
fn async_mode_cycles_collapse_and_recolor() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh_async(Dsr::Immediate, None);
    t.run("true");
    t.wait_settled(Duration::from_millis(300));
    t.run("false");
    t.wait_settled(Duration::from_millis(300));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true", "\u{276f} false"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} false"),
        RED,
        "{}",
        t.dump()
    );
}

/// THE async race: cycle N serves the cache and spawns a refresh; the
/// user has already typed `cd /`, so cycle N+1 paints a new-directory
/// prompt — and THEN cycle N's refresh lands, `zle reset-prompt`ing the
/// OLD directory's render over it. The 150 ms render delay makes the
/// ordering deterministic. The callback must discard results from
/// superseded cycles.
#[test]
fn async_stale_refresh_must_not_overwrite_newer_prompt() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh_async(Dsr::Immediate, Some(Duration::from_millis(150)));
    t.send("true\rcd /\r");
    t.wait_for("both cycles to collapse and reprompt", |s| {
        chevron_rows(s).len() >= 2 && prompt_ready(s)
    });
    // Generous settle: the stale refresh lands ~150 ms after its spawn,
    // plus the repaint it may trigger.
    t.wait_settled(Duration::from_millis(700));

    let prompt = t.with_screen(last_nonempty);
    assert!(
        prompt.contains(" / "),
        "live prompt must show the new directory: {prompt:?}\n{}",
        t.dump()
    );
    assert!(
        !prompt.contains(" ~ "),
        "a stale async refresh repainted the OLD directory's prompt: {prompt:?}\n{}",
        t.dump()
    );
}

/// Rapid typed-ahead commands under async: cached instant prompts,
/// background refreshes, and transient collapses interleave; counts and
/// the final prompt must stay exact.
#[test]
fn async_rapid_typeahead_stays_consistent() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh_async(Dsr::Immediate, None);
    t.send("true\rtrue\r");
    t.wait_for("both commands to collapse and reprompt", |s| {
        chevron_rows(s).len() >= 2 && prompt_ready(s)
    });
    t.wait_settled(Duration::from_millis(400));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true", "\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(t.with_screen(chevron_glyphs), 2, "{}", t.dump());
    let prompt = t.with_screen(last_nonempty);
    assert!(
        prompt.contains(LIVE_PROMPT_MARK) && !prompt.contains(CHEVRON),
        "refresh repaints must not disturb the live prompt: {prompt:?}\n{}",
        t.dump()
    );
}

/// tmux/SSH-grade latency that still fits the 300 ms read budget: the
/// rewrite must work exactly as in the immediate case.
#[test]
fn tmux_like_dsr_latency_still_rewrites() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Delayed(Duration::from_millis(80)));
    t.run("true");
    t.wait_settled(Duration::from_millis(300));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        GREEN,
        "{}",
        t.dump()
    );
}

/// Latency past the 300 ms `read -t` budget: the preexec query times out,
/// so the rewrite is skipped (honest neutral chevron) — but the response
/// still arrives *later*, while the shell is back at ZLE. It must not
/// surface as typed garbage or extra chevrons.
#[test]
fn dsr_slower_than_read_budget_degrades_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Delayed(Duration::from_millis(500)));
    t.run("true");
    // Long settle: the straggler response lands ~500 ms after the query.
    t.wait_settled(Duration::from_millis(900));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(rows, vec!["\u{276f} true"], "{}", t.dump());
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        vt100::Color::Default,
        "rewrite must be skipped (neutral chevron) when DSR times out\n{}",
        t.dump()
    );
    let prompt = t.with_screen(last_nonempty);
    assert!(
        prompt.contains(LIVE_PROMPT_MARK) && prompt.ends_with('\u{e0b0}'),
        "stale DSR response must not leak into the line editor: {prompt:?}\n{}",
        t.dump()
    );
}

/// A terminal that never answers DSR: every query burns the full 300 ms
/// timeout, but the shell must stay correct — neutral chevron, no
/// duplicates, no hang.
#[test]
fn terminal_without_dsr_degrades_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Silent);
    t.run("true");
    t.wait_settled(Duration::from_millis(400));

    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "{}",
        t.dump()
    );
    assert_eq!(
        t.chevron_color_on_row("\u{276f} true"),
        vt100::Color::Default,
        "{}",
        t.dump()
    );
}

/// The shell's stderr must stay wired to the terminal. Regression test:
/// the query helper opened its tty fd with a bare `exec {fd}< /dev/tty
/// 2>/dev/null` — and exec makes every redirection on it permanent, so
/// the first prompt cycle pointed the shell's (and every child's) fd 2
/// at /dev/null. Error messages silently vanished from then on.
#[test]
fn command_stderr_stays_visible() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    // A full prompt cycle first, so the query helper's fd dance has run.
    t.run("true");
    t.send_line("ls /chevron-no-such-path");
    t.wait_for("ls's error text to reach the terminal", |s| {
        lines(s).iter().any(|l| l.starts_with("ls:"))
    });
}

/// `reset` (ncurses tset) reads the terminal via stderr and
/// re-initializes it with RIS (`\ec`), which clears the screen and homes
/// the cursor between preexec's row save and precmd's rewrite. The
/// rewrite must skip (the homed cursor sits above the saved row, so the
/// delta goes negative), a fresh prompt must paint on the cleared
/// screen, and the shell must stay fully usable afterwards.
///
/// Regression test: with the shell's stderr left on /dev/null by the
/// bare-exec bug above, tset's `tcgetattr(STDERR_FILENO)` failed with
/// ENOTTY and it exited 1 before emitting a single byte — "the reset
/// command no longer works", with even its error message swallowed.
#[test]
fn reset_command_reinitializes_cleanly() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("/bin/echo before-reset");
    t.send_line("reset");
    t.wait_for("RIS to clear the screen and a fresh prompt", |s| {
        prompt_ready(s) && !lines(s).iter().any(|l| l.contains("before-reset"))
    });
    t.run("/bin/echo after-reset");
    t.wait_settled(Duration::from_millis(200));

    let all = t.with_screen(lines);
    assert!(
        all.iter().any(|l| l == "after-reset"),
        "shell must stay usable after reset\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} /bin/echo after-reset"],
        "only the post-reset command's collapsed row should remain\n{}",
        t.dump()
    );
}

/// Interactive secret entry (`read -rs "VAR?prompt"`) with paste
/// leftovers: the paste carries bytes past the newline the read
/// consumed, with no trailing newline of their own. A cooked-mode
/// pending-input probe cannot see a partial line (canonical select
/// reports readability only at line boundaries), so precmd's query
/// used to race the leftovers: `read -d 'R'` truncated at an `R`
/// inside them, ate everything before it, and left the real DSR
/// response stranded to surface as literal `^[[row;1R` garbage — the
/// shape reported with an Ashby API key paste. The probe now runs raw
/// (partials visible, query skipped) and truncated typeahead is
/// re-injected instead of eaten.
#[test]
fn paste_leftovers_during_interactive_read_stay_clean() {
    if !zsh_available() {
        return;
    }
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.send_line("read -rs \"K?key: \"; echo len-${#K}");
    t.wait_for("the read builtin's prompt", |s| {
        lines(s).iter().any(|l| l == "key:")
    });
    // Paste: the secret, the newline that finishes the read, then a
    // tail with an interior `R` that stays queued as a partial line.
    t.send("SECRETKEY9\rTRAILR-MORE");
    t.wait_for("read to finish with the key intact", |s| {
        lines(s).iter().any(|l| l.contains("len-10"))
    });
    t.wait_settled(Duration::from_millis(400));

    let all = t.with_screen(lines);
    assert!(
        all.iter().all(|l| !l.contains(";1") && !l.contains("^[")),
        "no DSR response may leak as literal text\n{}",
        t.dump()
    );
    let prompt_row = t.with_screen(last_nonempty);
    assert!(
        prompt_row.ends_with("TRAILR-MORE"),
        "paste leftovers must reach the edit buffer intact, not be eaten\n{}",
        t.dump()
    );
}

/// An emulator that answers preexec's query instantly but precmd's
/// rewrite query past the 300 ms read budget. preexec's stragglers are
/// covered by the sweep at the top of the NEXT precmd — but behind
/// precmd's own query there is no sweep, only ZLE: the response used to
/// arrive just after the helper restored echo and kernel-echo at the
/// cursor as literal `^[[row;1R` (or self-insert its tail into the
/// edit buffer). The helper now lingers in its raw window on timeout
/// and absorbs the straggler before echo returns.
#[test]
fn slow_precmd_dsr_response_never_leaks() {
    if !zsh_available() {
        return;
    }
    // The straggler lands at 400 ms: 100 ms past the read budget (the
    // helper has definitely given up) and squarely inside the 300 ms
    // render window the SyncDelayed wrapper holds open — echo is
    // restored, ZLE is not yet up, so an unabsorbed response
    // kernel-echoes. The helper's 300 ms linger covers arrivals
    // through ~600 ms.
    let t = Term::spawn_inner(
        Dsr::AlternateDelayed(Duration::from_millis(400)),
        ROWS,
        COLS,
        Render::SyncDelayed(Duration::from_millis(300)),
    );
    t.run("true");
    t.wait_settled(Duration::from_millis(700));

    // The leak is transient on screen (the next prompt can paint over
    // it), so assert on the raw byte stream: a kernel-echoed response
    // appears as caret-notation `^[[` — three printable bytes that
    // never occur in chevron's legitimate output (real CSIs are ESC
    // bytes, not caret text).
    let echoed_caret = {
        let raw = t.raw.lock().unwrap();
        raw.windows(3).any(|w| w == b"^[[")
    };
    assert!(
        !echoed_caret,
        "straggling precmd response must never kernel-echo\n{}",
        t.dump()
    );
    assert_eq!(
        t.with_screen(chevron_rows),
        vec!["\u{276f} true"],
        "{}",
        t.dump()
    );
}
