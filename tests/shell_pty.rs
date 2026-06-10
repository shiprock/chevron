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
        // Prepend path *in .zshrc* so it wins over anything /etc/zshenv
        // (nix-darwin, etc.) prepends after CommandBuilder's env.
        std::fs::write(
            home_path.join(".zshrc"),
            format!(
                "path=({} $path)\neval \"$(chevron init zsh)\"\n",
                bin_dir.display()
            ),
        )
        .unwrap();

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
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
        ] {
            cmd.env_remove(var);
        }

        let child = pair.slave.spawn_command(cmd).unwrap();
        // Close our copy of the slave so master reads EOF once zsh exits.
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 200)));
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
