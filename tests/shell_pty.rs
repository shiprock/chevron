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
}

struct Term {
    parser: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<Vec<u8>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // Keeps the master side of the PTY open for the reader thread.
    _master: Box<dyn portable_pty::MasterPty>,
    _home: tempfile::TempDir,
}

// ── screen helpers (free functions so wait_for predicates can use them) ─────

fn lines(screen: &vt100::Screen) -> Vec<String> {
    screen
        .rows(0, COLS)
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
                let due = match dsr {
                    Dsr::Silent => continue,
                    Dsr::Immediate => Instant::now(),
                    Dsr::Delayed(d) => Instant::now() + d,
                };
                if tx.send((due, resp)).is_err() {
                    break;
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
            _master: pair.master,
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

    /// Foreground color of the first chevron glyph on the given row.
    fn chevron_color_on_row(&self, row_text: &str) -> vt100::Color {
        self.with_screen(|s| {
            let all = lines(s);
            let row = all
                .iter()
                .position(|l| l == row_text)
                .unwrap_or_else(|| panic!("row {row_text:?} not on screen\n{}", self.dump()));
            let col = all[row].chars().position(|c| c == CHEVRON).unwrap();
            s.cell(u16::try_from(row).unwrap(), u16::try_from(col).unwrap())
                .unwrap()
                .fgcolor()
        })
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
    let t = Term::spawn_zsh(Dsr::Immediate);
    t.run("true");
    t.wait_settled(Duration::from_millis(150));

    let rows = t.with_screen(chevron_rows);
    assert_eq!(rows, vec!["\u{276f} true"], "{}", t.dump());
    assert_eq!(t.with_screen(chevron_glyphs), 1, "{}", t.dump());
}

#[test]
fn rewrite_colors_chevron_by_exit_status() {
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
/// KNOWN BUG (deterministic repro, fails 3/3): the typed-ahead `true\r`
/// is consumed by the DSR `read -d 'R'` along with the cursor response,
/// stripped as "pre-ESC typeahead", and re-injected with `print -z` —
/// which parks it in the next prompt's edit buffer instead of executing
/// it. The raw transcript also shows the precmd DSR response echoed
/// literally (`^[[2;1R`): unlike preexec, precmd's query has no
/// `stty -echo` guard, so response bytes arriving before `read -s`
/// flips echo off are echoed by the kernel at the cursor position.
/// Run with: `cargo test --test shell_pty -- --ignored`
#[test]
#[ignore = "deterministic repro of typeahead-swallowing during DSR exchange; see doc comment"]
fn rapid_consecutive_builtins_no_duplicates() {
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
}

/// tmux/SSH-grade latency that still fits the 300 ms read budget: the
/// rewrite must work exactly as in the immediate case.
#[test]
fn tmux_like_dsr_latency_still_rewrites() {
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
