//! The banner as text: a card drawn with box characters in the colours of
//! the tmux theme it sits under (badwolf's greys, accent colour 39). Unlike
//! the image banner it needs no graphics protocol: it is drawn in the
//! terminal's own font, passes through tmux untouched, and fits any terminal
//! from 40 columns up, at 80 at most as the image was. It names no host:
//! a machine's name can say whose it is.
//!
//! ```text
//! ╭─ TERMINAL UNDERGROUND DIVISION ───────────── macOS · aarch64 · 192.168.2.30 ─╮
//! │                                                                              │
//! │  CPU  ██▎           19%    MEM  █████▋        47%    DSK  ████▏         34%  │
//! │  LOD  2.66 3.46 3.27       UPT  14d 21h              PRC  1332               │
//! │                                                                              │
//! ╰─────────────────────────────────────────────── the underground never sleeps ─╯
//! ```

use std::io::{self, Write};

use super::render::{format_uptime, tagline_of_the_day};
use crate::color::{BOLD, RST, UNBOLD, bg, fg};
use crate::sysinfo::SystemInfo;

/// The card's widest, and the narrowest terminal it is drawn in.
const MAX_WIDTH: usize = 80;
const MIN_WIDTH: usize = 40;
/// The narrowest card with its gauges side by side, four cells each.
const MIN_WIDE: usize = 56;
/// Columns before a row's first item, and between the side-by-side columns.
const INDENT: usize = 2;
const GAP: usize = 4;
/// A gauge's width beyond its cells: the label, two spaces, one, " 19%".
const GAUGE_EXTRA: usize = 10;

// tmux.conf's badwolf colours.
const BORDER: u8 = 240;
const FAINT: u8 = 238;
const DIM: u8 = 245;
const HI: u8 = 254;
const ACCENT: u8 = 39;
/// A gauge's empty cells.
const TRACK: u8 = 236;
/// Its fill, blended cell by cell from deep blue (colour 31) at its start
/// to the accent (colour 39) at its end. A blend in 24-bit colour: the
/// 256-colour blues between them step visibly, in bands.
const FILL_FROM: [u8; 3] = [0x00, 0x87, 0xaf];
const FILL_TO: [u8; 3] = [0x00, 0xaf, 0xff];
/// A gauge past 70% turns yellow, past 90% red.
const WARN: u8 = 221;
const CRIT: u8 = 196;

const ITALIC: &str = "\x1b[3m";
const UNITALIC: &str = "\x1b[23m";
const DEFAULT_BG: &str = "\x1b[49m";
/// A gauge's last cell, one to seven eighths full.
const EIGHTHS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];
const TITLE: &str = "TERMINAL UNDERGROUND DIVISION";

/// Draw the card for this machine on stdout, sized to the terminal.
pub fn print() {
    let lines = render(
        &SystemInfo::gather(),
        tagline_of_the_day(),
        terminal_columns(),
    );
    let mut out = io::stdout().lock();
    for line in lines {
        // A closed stdout (the shell went away) is no reason to fail.
        if writeln!(out, "{line}").is_err() {
            return;
        }
    }
}

/// The terminal's width: stdout's, else stderr's, else `$COLUMNS`, else 80.
fn terminal_columns() -> usize {
    for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes a winsize into the local. On a descriptor
        // that is not a terminal it fails and leaves the local as it was.
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut ws) };
        if rc == 0 && ws.ws_col > 0 {
            return usize::from(ws.ws_col);
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|&c| c > 0)
        .unwrap_or(MAX_WIDTH)
}

/// The card for `info` in a terminal `columns` wide, one string per line,
/// each exactly as wide as the card. No card below 40 columns.
fn render(info: &SystemInfo, tagline: &str, columns: usize) -> Vec<String> {
    if columns < MIN_WIDTH {
        return Vec::new();
    }
    let width = columns.min(MAX_WIDTH);
    let figures = Figures::of(info);
    let mut lines = vec![top_edge(width, &figures)];
    lines.extend(body(width, &figures));
    lines.push(bottom_edge(width, tagline));
    lines.into_iter().map(|line| line.text).collect()
}

/// What the card shows, read off `SystemInfo`: the gauges as the image
/// banner computes them (CPU as the 1-minute load over the cores).
struct Figures {
    platform: Vec<String>,
    cpu: u8,
    mem: u8,
    dsk: u8,
    load: String,
    uptime: String,
    procs: String,
}

impl Figures {
    #[allow(clippy::cast_precision_loss)]
    fn of(info: &SystemInfo) -> Self {
        let mut platform = vec![os_name(info.os).to_string(), info.arch.to_string()];
        if !info.ip_addr.is_empty() {
            platform.push(info.ip_addr.clone());
        }
        Self {
            platform,
            cpu: percent(info.load_1, f64::from(info.ncores.max(1))),
            mem: percent(info.mem_used as f64, info.mem_total as f64),
            dsk: percent(info.disk_used as f64, info.disk_total as f64),
            load: format!("{:.2} {:.2} {:.2}", info.load_1, info.load_5, info.load_15),
            uptime: format_uptime(info.uptime_secs),
            procs: info.proc_count.to_string(),
        }
    }
}

/// `part` of `whole` as a whole percentage, 0 to 100.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn percent(part: f64, whole: f64) -> u8 {
    if whole <= 0.0 {
        return 0;
    }
    (part / whole * 100.0).round().clamp(0.0, 100.0) as u8
}

/// `std::env::consts::OS` as people write it.
fn os_name(os: &str) -> &str {
    match os {
        "macos" => "macOS",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        "netbsd" => "NetBSD",
        other => other,
    }
}

/// A line being built: its text with escapes, and its width on screen. Every
/// character the card draws takes one column.
#[derive(Default)]
struct Line {
    text: String,
    width: usize,
}

impl Line {
    fn put(&mut self, s: &str) -> &mut Self {
        self.text.push_str(s);
        self.width += s.chars().count();
        self
    }

    fn style(&mut self, escape: &str) -> &mut Self {
        self.text.push_str(escape);
        self
    }

    fn append(&mut self, other: &Line) -> &mut Self {
        self.text.push_str(&other.text);
        self.width += other.width;
        self
    }

    fn pad_to(&mut self, column: usize) -> &mut Self {
        while self.width < column {
            self.text.push(' ');
            self.width += 1;
        }
        self
    }
}

/// `s` cut to `max` columns, with an ellipsis where it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `╭─ left ───…─── right ─╮`, or `╰…╯` along the bottom; `None` when the
/// labels leave no room for a rule between them.
fn edge(width: usize, top: bool, left: Option<&Line>, right: Option<&Line>) -> Option<Line> {
    let (open, close) = if top {
        ("╭─", "─╮")
    } else {
        ("╰─", "─╯")
    };
    let room = |label: Option<&Line>| label.map_or(0, |l| l.width + 2);
    let rule = width
        .checked_sub(open.chars().count() + room(left) + room(right) + close.chars().count())
        .filter(|&n| n > 0)?;
    let mut line = Line::default();
    line.style(fg(BORDER)).put(open);
    if let Some(label) = left {
        line.put(" ").append(label).style(fg(BORDER)).put(" ");
    }
    line.put(&"─".repeat(rule));
    if let Some(label) = right {
        line.put(" ").append(label).style(fg(BORDER)).put(" ");
    }
    line.put(close).style(RST);
    Some(line)
}

/// The title in the accent on the left; on the right the platform, as much
/// of it as fits (the address goes first, then the rest).
fn top_edge(width: usize, f: &Figures) -> Line {
    let mut title = Line::default();
    title.style(BOLD).style(fg(ACCENT)).put(TITLE).style(UNBOLD);
    for shown in (1..=f.platform.len()).rev() {
        let mut platform = Line::default();
        for (i, part) in f.platform.iter().take(shown).enumerate() {
            if i > 0 {
                platform.style(fg(FAINT)).put(" · ");
            }
            platform.style(fg(DIM)).put(part);
        }
        if let Some(line) = edge(width, true, Some(&title), Some(&platform)) {
            return line;
        }
    }
    edge(width, true, Some(&title), None)
        .or_else(|| edge(width, true, None, None))
        .unwrap_or_default()
}

/// The day's tagline on the right, cut if the card is too narrow for it.
fn bottom_edge(width: usize, tagline: &str) -> Line {
    let label = |text: &str| {
        let mut l = Line::default();
        l.style(ITALIC).style(fg(DIM)).put(text).style(UNITALIC);
        l
    };
    let full = label(tagline);
    edge(width, false, None, Some(&full))
        .or_else(|| {
            let cut = label(&truncate(tagline, width.saturating_sub(7)));
            edge(width, false, None, Some(&cut))
        })
        .or_else(|| edge(width, false, None, None))
        .unwrap_or_default()
}

/// The rows between the edges: the gauges and the figures, side by side in
/// three columns where they fit and one per row where they do not, with a
/// blank row above and below.
fn body(width: usize, f: &Figures) -> Vec<Line> {
    let inner = width - 2;
    let mut rows = vec![Line::default()];
    match side_by_side(width, f) {
        Some(pair) => rows.extend(pair),
        None => rows.extend(stacked(width, f)),
    }
    rows.push(Line::default());
    rows.into_iter()
        .map(|content| {
            let mut line = Line::default();
            line.style(fg(BORDER))
                .put("│")
                .style(RST)
                .append(&content)
                .style(RST);
            line.pad_to(inner + 1).style(fg(BORDER)).put("│").style(RST);
            line
        })
        .collect()
}

/// CPU, MEM and DSK side by side with load, uptime and processes under them,
/// or `None` when the card is too narrow or a figure runs into the next one.
fn side_by_side(width: usize, f: &Figures) -> Option<[Line; 2]> {
    if width < MIN_WIDE {
        return None;
    }
    let cells = ((width - (INDENT + 2 * GAP + 3 * GAUGE_EXTRA + 4)) / 3).min(12);
    let step = cells + GAUGE_EXTRA + GAP;
    let columns = [INDENT, INDENT + step, INDENT + 2 * step];
    let mut gauges = Line::default();
    for (column, (label, pct)) in
        columns
            .iter()
            .zip([("CPU", f.cpu), ("MEM", f.mem), ("DSK", f.dsk)])
    {
        gauges.pad_to(*column);
        gauge(&mut gauges, label, pct, cells);
    }
    let mut figures = Line::default();
    for (column, (label, value)) in
        columns
            .iter()
            .zip([("LOD", &f.load), ("UPT", &f.uptime), ("PRC", &f.procs)])
    {
        if figures.width > 0 && figures.width + 2 > *column {
            return None;
        }
        figures.pad_to(*column);
        figure(&mut figures, label, value);
    }
    (figures.width + 2 <= width - 2).then_some([gauges, figures])
}

/// One gauge or figure per row, the gauges as wide as the card allows.
fn stacked(width: usize, f: &Figures) -> Vec<Line> {
    let cells = width - 2 - INDENT - GAUGE_EXTRA - 2;
    let mut rows = Vec::new();
    for (label, pct) in [("CPU", f.cpu), ("MEM", f.mem), ("DSK", f.dsk)] {
        let mut row = Line::default();
        row.pad_to(INDENT);
        gauge(&mut row, label, pct, cells);
        rows.push(row);
    }
    for (label, value) in [("LOD", &f.load), ("UPT", &f.uptime), ("PRC", &f.procs)] {
        let mut row = Line::default();
        row.pad_to(INDENT);
        figure(&mut row, label, value);
        rows.push(row);
    }
    rows
}

/// `CPU  ██▎         19%`: a bar of `cells` on a dark track, filled to
/// the eighth of a cell, its blue deepening toward its start (yellow past
/// 70%, red past 90%), then the percentage.
fn gauge(line: &mut Line, label: &str, pct: u8, cells: usize) {
    line.style(fg(DIM)).put(label).put("  ").style(bg(TRACK));
    let eighths = (usize::from(pct) * cells * 8 + 50) / 100;
    let (full, part) = (eighths / 8, eighths % 8);
    for i in 0..cells {
        match pct {
            91.. => line.style(fg(CRIT)),
            71..=90 => line.style(fg(WARN)),
            _ => line.style(&fill(i, cells)),
        };
        if i < full {
            line.put("█");
        } else if i == full && part > 0 {
            line.put(EIGHTHS[part - 1]);
        } else {
            line.put(" ");
        }
    }
    line.style(DEFAULT_BG)
        .put(" ")
        .style(fg(HI))
        .put(&format!("{pct:>3}%"));
}

/// The foreground escape for cell `i` of a gauge `cells` long: its share
/// of the way from `FILL_FROM` to `FILL_TO`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn fill(i: usize, cells: usize) -> String {
    let share = if cells > 1 {
        i as f32 / (cells - 1) as f32
    } else {
        1.0
    };
    let [red, green, blue] = [0, 1, 2].map(|channel| {
        let from = f32::from(FILL_FROM[channel]);
        let to = f32::from(FILL_TO[channel]);
        (from + (to - from) * share).round() as u8
    });
    format!("\x1b[38;2;{red};{green};{blue}m")
}

/// `UPT  14d 21h`: a label and its value.
fn figure(line: &mut Line, label: &str, value: &str) {
    line.style(fg(DIM))
        .put(label)
        .put("  ")
        .style(fg(HI))
        .put(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> SystemInfo {
        SystemInfo {
            hostname: "mim-moab".to_string(),
            os: "macos",
            arch: "aarch64",
            date: String::new(),
            ncores: 14,
            load_1: 2.66,
            load_5: 3.46,
            load_15: 3.27,
            mem_total: 100,
            mem_used: 47,
            mem_wired: 0,
            mem_compressed: 0,
            disk_total: 100,
            disk_used: 34,
            uptime_secs: 14 * 86_400 + 21 * 3_600,
            proc_count: 1332,
            ip_addr: "192.168.2.30".to_string(),
        }
    }

    const TAGLINE: &str = "the underground never sleeps";

    /// The text a line shows, its escapes left out.
    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn plain_lines(info: &SystemInfo, tagline: &str, columns: usize) -> Vec<String> {
        render(info, tagline, columns)
            .iter()
            .map(|l| plain(l))
            .collect()
    }

    #[test]
    fn the_card_at_80_columns() {
        let lines = plain_lines(&info(), TAGLINE, 80);
        let expected = [
            "╭─ TERMINAL UNDERGROUND DIVISION ───────────── macOS · aarch64 · 192.168.2.30 ─╮",
            "│                                                                              │",
            "│  CPU  ██▎           19%    MEM  █████▋        47%    DSK  ████▏         34%  │",
            "│  LOD  2.66 3.46 3.27       UPT  14d 21h              PRC  1332               │",
            "│                                                                              │",
            "╰─────────────────────────────────────────────── the underground never sleeps ─╯",
        ];
        assert_eq!(lines, expected);
    }

    #[test]
    fn every_line_is_as_wide_as_the_card() {
        for columns in [40, 41, 47, 48, 55, 56, 57, 63, 64, 79, 80, 81, 200] {
            let lines = plain_lines(&info(), TAGLINE, columns);
            assert!(!lines.is_empty(), "no card at {columns} columns");
            for line in &lines {
                assert_eq!(
                    line.chars().count(),
                    columns.min(80),
                    "at {columns} columns: {line:?}"
                );
            }
        }
    }

    #[test]
    fn no_card_below_40_columns() {
        assert_eq!(render(&info(), TAGLINE, 39), Vec::<String>::new());
        assert_eq!(render(&info(), TAGLINE, 0), Vec::<String>::new());
    }

    #[test]
    fn a_narrow_card_stacks_its_gauges() {
        let lines = plain_lines(&info(), TAGLINE, 48);
        assert!(lines[2].starts_with("│  CPU  "), "{lines:#?}");
        assert!(lines[3].starts_with("│  MEM  "), "{lines:#?}");
        assert!(lines[4].starts_with("│  DSK  "), "{lines:#?}");
        assert!(lines[7].starts_with("│  PRC  1332"), "{lines:#?}");
    }

    #[test]
    fn the_top_edge_drops_the_address_first_then_the_rest() {
        let top = &plain_lines(&info(), TAGLINE, 56)[0];
        assert!(top.ends_with(" macOS · aarch64 ─╮"), "{top}");
        let top = &plain_lines(&info(), TAGLINE, 48)[0];
        assert!(top.ends_with(" macOS ─╮"), "{top}");
        let top = &plain_lines(&info(), TAGLINE, 40)[0];
        assert!(
            top.starts_with("╭─ TERMINAL UNDERGROUND DIVISION ─"),
            "{top}"
        );
        assert!(!top.contains("macOS"), "{top}");
    }

    #[test]
    fn the_card_never_names_the_host() {
        for columns in [40, 56, 80] {
            for line in plain_lines(&info(), TAGLINE, columns) {
                assert!(!line.to_lowercase().contains("moab"), "{line}");
            }
        }
    }

    #[test]
    fn without_an_address_the_platform_ends_at_the_architecture() {
        let mut info = info();
        info.ip_addr.clear();
        let top = &plain_lines(&info, TAGLINE, 80)[0];
        assert!(top.ends_with(" macOS · aarch64 ─╮"), "{top}");
    }

    #[test]
    fn a_tagline_too_long_for_the_card_is_cut() {
        let long = "another fine release from the underground";
        let bottom = plain_lines(&info(), long, 40).pop().unwrap();
        assert!(bottom.ends_with("… ─╯"), "{bottom}");
        assert_eq!(bottom.chars().count(), 40);
    }

    #[test]
    fn gauges_turn_yellow_past_70_and_red_past_90() {
        let mut info = info();
        info.mem_used = 75;
        info.disk_used = 95;
        let row = &render(&info, TAGLINE, 80)[2];
        assert!(row.contains(&format!("{}█", fg(WARN))), "{row:?}");
        assert!(row.contains(&format!("{}█", fg(CRIT))), "{row:?}");
    }

    #[test]
    fn a_full_gauge_fills_every_cell() {
        let mut line = Line::default();
        gauge(&mut line, "CPU", 100, 12);
        assert_eq!(plain(&line.text), format!("CPU  {} 100%", "█".repeat(12)));
        let mut line = Line::default();
        gauge(&mut line, "CPU", 0, 12);
        assert_eq!(plain(&line.text), format!("CPU  {}   0%", " ".repeat(12)));
    }

    #[test]
    fn the_fill_blends_from_deep_blue_to_the_accent() {
        let mut line = Line::default();
        gauge(&mut line, "CPU", 50, 12);
        assert!(
            line.text.contains("\x1b[38;2;0;135;175m█"),
            "{:?}",
            line.text
        );
        assert_eq!(fill(11, 12), "\x1b[38;2;0;175;255m");
        assert_eq!(fill(0, 1), "\x1b[38;2;0;175;255m");
    }

    #[test]
    fn the_cpu_gauge_is_the_load_over_the_cores() {
        let mut info = info();
        info.load_1 = 28.0; // twice the cores: a full gauge, not 200%
        assert_eq!(Figures::of(&info).cpu, 100);
        info.ncores = 0; // never divides by zero
        assert_eq!(Figures::of(&info).cpu, 100);
    }

    #[test]
    fn os_names_read_as_written() {
        assert_eq!(os_name("macos"), "macOS");
        assert_eq!(os_name("linux"), "Linux");
        assert_eq!(os_name("plan9"), "plan9");
    }
}
