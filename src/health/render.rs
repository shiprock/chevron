use std::fmt::Write;

use super::check::{Check, Severity};

const BOX_WIDTH: usize = 70;

pub fn render(checks: &[Check], color: bool) -> String {
    let mut out = String::with_capacity(2048);
    render_header(&mut out, color);
    render_checks(&mut out, checks, color);
    render_recommendations(&mut out, checks, color);
    out
}

fn render_header(out: &mut String, color: bool) {
    let bold_on = if color { "\x1b[1m" } else { "" };
    let bold_off = if color { "\x1b[0m" } else { "" };
    let inner_width = BOX_WIDTH - 2;
    let bar = "═".repeat(inner_width);
    let _ = writeln!(out, "╔{bar}╗");
    let _ = writeln!(out, "║{}║", " ".repeat(inner_width));
    let title = "System Health Report";
    let pad_right = inner_width.saturating_sub(2 + title.len());
    let _ = writeln!(
        out,
        "║  {bold_on}{title}{bold_off}{}║",
        " ".repeat(pad_right)
    );
    let _ = writeln!(out, "║{}║", " ".repeat(inner_width));
    let _ = writeln!(out, "╚{bar}╝");
    let _ = writeln!(out);
}

fn render_checks(out: &mut String, checks: &[Check], color: bool) {
    let label_width = checks.iter().map(|c| c.label.len()).max().unwrap_or(0);
    for check in checks {
        let (yel, red, rst) = ansi(color);
        let pad = " ".repeat(label_width.saturating_sub(check.label.len()));
        let _ = write!(
            out,
            "  {yel}{label}:{rst}{pad} {value}",
            label = check.label,
            value = check.value,
        );
        if let (Some(hint), Some(note_color)) =
            (check.hint.as_deref(), severity_color(check.severity, color))
        {
            let _ = write!(out, " {note_color}({hint}){rst}");
        }
        // Mark Critical even without hint
        if check.severity == Severity::Critical && check.hint.is_none() && color {
            let _ = write!(out, " {red}(critical){rst}");
        }
        let _ = writeln!(out);
    }
}

fn render_recommendations(out: &mut String, checks: &[Check], color: bool) {
    let hints: Vec<&str> = checks
        .iter()
        .filter_map(|c| match c.severity {
            Severity::Warn | Severity::Critical => c.hint.as_deref(),
            _ => None,
        })
        .collect();
    if hints.is_empty() {
        return;
    }
    let bold_on = if color { "\x1b[1m" } else { "" };
    let bold_off = if color { "\x1b[0m" } else { "" };
    let inner_width = BOX_WIDTH - 2;
    let bar = "─".repeat(inner_width);
    let _ = writeln!(out);
    let _ = writeln!(out, "┌{bar}┐");
    let label = "RECOMMENDATIONS";
    let pad = inner_width.saturating_sub(2 + label.len());
    let _ = writeln!(out, "│  {bold_on}{label}{bold_off}{}│", " ".repeat(pad));
    let _ = writeln!(out, "├{bar}┤");
    for hint in hints {
        let line = format!("  - {hint}");
        let pad = inner_width.saturating_sub(line.len());
        let _ = writeln!(out, "│{line}{}│", " ".repeat(pad));
    }
    let _ = writeln!(out, "└{bar}┘");
}

fn ansi(color: bool) -> (&'static str, &'static str, &'static str) {
    if color {
        ("\x1b[33m", "\x1b[31m", "\x1b[0m")
    } else {
        ("", "", "")
    }
}

fn severity_color(sev: Severity, color: bool) -> Option<&'static str> {
    if !color {
        return Some("");
    }
    match sev {
        Severity::Warn => Some("\x1b[33m"),
        Severity::Critical => Some("\x1b[31m"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::check::Check;
    use super::render;

    #[test]
    fn no_color_strips_escapes() {
        let checks = vec![Check::ok("load", "Load", "0.1")];
        let out = render(&checks, false);
        assert!(!out.contains('\x1b'));
        assert!(out.contains("Load:"));
        assert!(out.contains("0.1"));
    }

    #[test]
    fn color_includes_escapes() {
        let checks = vec![Check::ok("load", "Load", "0.1")];
        let out = render(&checks, true);
        assert!(out.contains('\x1b'));
    }

    #[test]
    fn warn_hint_appears_in_recommendations_box() {
        let checks = vec![Check::warn("mem", "Memory", "91%", "free up memory")];
        let out = render(&checks, false);
        assert!(out.contains("RECOMMENDATIONS"));
        assert!(out.contains("free up memory"));
    }

    #[test]
    fn no_recommendations_box_when_all_ok() {
        let checks = vec![Check::ok("load", "Load", "0.1")];
        let out = render(&checks, false);
        assert!(!out.contains("RECOMMENDATIONS"));
    }

    #[test]
    fn unknown_does_not_trigger_recommendations() {
        let checks = vec![Check::unknown("foo", "Foo", "unknown")];
        let out = render(&checks, false);
        assert!(!out.contains("RECOMMENDATIONS"));
    }
}
