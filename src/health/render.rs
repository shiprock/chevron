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

/// Render a single check as one human-readable line (no trailing newline).
pub fn render_line(check: &Check, color: bool) -> String {
    let mut out = String::with_capacity(128);
    write_line(&mut out, check, check.label.len(), color);
    out
}

/// Just the value field, plain text, with trailing newline.
pub fn render_value(check: &Check) -> String {
    format!("{}\n", check.value)
}

/// JSON for a list of checks. If `single_object` and `checks.len() == 1`,
/// emit the check directly (no wrapper). Otherwise wrap as `{"checks":[...]}`.
pub fn render_json(checks: &[Check], single_object: bool) -> String {
    if single_object && checks.len() == 1 {
        let mut out = check_to_json(&checks[0]);
        out.push('\n');
        return out;
    }
    let body: Vec<String> = checks.iter().map(check_to_json).collect();
    format!("{{\"checks\":[{}]}}\n", body.join(","))
}

fn check_to_json(c: &Check) -> String {
    let hint = match &c.hint {
        Some(h) => format!("\"{}\"", json_escape(h)),
        None => "null".to_string(),
    };
    format!(
        "{{\"name\":\"{}\",\"label\":\"{}\",\"value\":\"{}\",\"severity\":\"{}\",\"hint\":{}}}",
        json_escape(c.name),
        json_escape(c.label),
        json_escape(&c.value),
        severity_str(c.severity),
        hint,
    )
}

fn severity_str(sev: Severity) -> &'static str {
    match sev {
        Severity::Ok => "ok",
        Severity::Warn => "warn",
        Severity::Critical => "critical",
        Severity::Unknown => "unknown",
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
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
        write_line(out, check, label_width, color);
        out.push('\n');
    }
}

fn write_line(out: &mut String, check: &Check, label_width: usize, color: bool) {
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
    if check.severity == Severity::Critical && check.hint.is_none() && color {
        let _ = write!(out, " {red}(critical){rst}");
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
    use super::{json_escape, render, render_json, render_line, render_value};

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

    // ── new phase 3 surfaces ────────────────────────────────────────────────

    #[test]
    fn render_line_includes_label_and_value() {
        let c = Check::ok("load", "System Load", "0.42");
        let out = render_line(&c, false);
        assert!(out.contains("System Load:"));
        assert!(out.contains("0.42"));
        assert!(!out.ends_with('\n'));
    }

    #[test]
    fn render_value_is_just_the_value() {
        let c = Check::ok("load", "System Load", "0.42");
        assert_eq!(render_value(&c), "0.42\n");
    }

    #[test]
    fn json_single_object_has_no_wrapper() {
        let c = Check::ok("load", "System Load", "0.42");
        let out = render_json(std::slice::from_ref(&c), true);
        assert!(out.starts_with('{'));
        assert!(!out.contains("\"checks\""));
        assert!(out.contains("\"name\":\"load\""));
        assert!(out.contains("\"severity\":\"ok\""));
        assert!(out.contains("\"hint\":null"));
    }

    #[test]
    fn json_multi_uses_checks_wrapper() {
        let checks = vec![
            Check::ok("load", "Load", "0.1"),
            Check::warn("mem", "Memory", "91%", "free memory"),
        ];
        let out = render_json(&checks, false);
        assert!(out.starts_with("{\"checks\":["));
        assert!(out.contains("\"severity\":\"warn\""));
        assert!(out.contains("\"hint\":\"free memory\""));
    }

    #[test]
    fn json_escapes_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn json_escapes_control_chars() {
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\x01b"), "a\\u0001b");
    }

    #[test]
    fn json_severity_strings() {
        let cases = [
            (Check::ok("a", "A", "x"), "\"severity\":\"ok\""),
            (Check::warn("a", "A", "x", "h"), "\"severity\":\"warn\""),
            (
                Check::critical("a", "A", "x", "h"),
                "\"severity\":\"critical\"",
            ),
            (Check::unknown("a", "A", "x"), "\"severity\":\"unknown\""),
        ];
        for (check, expected) in cases {
            let out = render_json(std::slice::from_ref(&check), true);
            assert!(out.contains(expected), "missing {expected} in {out}");
        }
    }
}
