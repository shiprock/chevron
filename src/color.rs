use std::sync::LazyLock;

pub const ARROW: &str = "\u{E0B0}";
pub const THIN: &str = "\u{E0B1}";
pub const BRANCH_ICON: &str = "\u{E0A0}";
pub const PENCIL_ICON: &str = "\u{F040}";
pub const RST: &str = "\x1b[0m";
pub const BOLD: &str = "\x1b[1m";
pub const UNBOLD: &str = "\x1b[22m";

// Pre-built ANSI color tables. The 256-color palette is bounded, so we build
// every fg/bg escape once at first use and return borrowed slices thereafter.
// Per-render this eliminates dozens of `format!` allocations on the hot path.
static FG_CODES: LazyLock<[String; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| format!("\x1b[38;5;{i}m")));
static BG_CODES: LazyLock<[String; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| format!("\x1b[48;5;{i}m")));

/// Powerline arrow transition. `None` means first segment (no arrow glyph).
#[must_use]
pub fn arrow(from_bg: Option<u8>, to_bg: u8) -> String {
    let mut out = String::with_capacity(24);
    if let Some(prev) = from_bg {
        out.push_str(fg(prev));
        out.push_str(bg(to_bg));
        out.push_str(ARROW);
    } else {
        out.push_str(bg(to_bg));
    }
    out
}

#[must_use]
pub fn fg(color: u8) -> &'static str {
    &FG_CODES[color as usize]
}

#[must_use]
pub fn bg(color: u8) -> &'static str {
    &BG_CODES[color as usize]
}

/// Wrap ANSI escape sequences in `%{...%}` so zsh can calculate visible prompt width.
#[must_use]
pub fn zsh_wrap_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    let mut parts = s.split('\x1b');

    if let Some(first) = parts.next() {
        out.push_str(first);
    }

    for part in parts {
        if let Some(m_pos) = part.find('m') {
            out.push_str("%{\x1b");
            out.push_str(&part[..=m_pos]);
            out.push_str("%}");
            out.push_str(&part[m_pos + 1..]);
        } else {
            out.push('\x1b');
            out.push_str(part);
        }
    }

    out
}

/// Wrap ANSI escape sequences in `\x01...\x02` so bash readline can
/// calculate visible prompt width when PS1 is set programmatically.
#[must_use]
pub fn bash_wrap_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 4);
    let mut parts = s.split('\x1b');

    if let Some(first) = parts.next() {
        out.push_str(first);
    }

    for part in parts {
        if let Some(m_pos) = part.find('m') {
            out.push_str("\x01\x1b");
            out.push_str(&part[..=m_pos]);
            out.push('\x02');
            out.push_str(&part[m_pos + 1..]);
        } else {
            out.push('\x1b');
            out.push_str(part);
        }
    }

    out
}

/// Wrap ANSI escapes for the given shell. Falls back to zsh wrapping.
#[must_use]
pub fn wrap_for_shell(shell: &str, s: &str) -> String {
    match shell {
        "bash" => bash_wrap_escapes(s),
        "fish" => s.to_string(),
        _ => zsh_wrap_escapes(s),
    }
}

#[cfg(test)]
mod tests {
    use super::{ARROW, arrow, bash_wrap_escapes, bg, fg, wrap_for_shell, zsh_wrap_escapes};

    #[test]
    fn fg_produces_ansi_color() {
        assert_eq!(fg(31), "\x1b[38;5;31m");
        assert_eq!(fg(0), "\x1b[38;5;0m");
        assert_eq!(fg(255), "\x1b[38;5;255m");
    }

    #[test]
    fn bg_produces_ansi_color() {
        assert_eq!(bg(31), "\x1b[48;5;31m");
        assert_eq!(bg(0), "\x1b[48;5;0m");
    }

    #[test]
    fn zsh_wrap_no_escapes() {
        assert_eq!(zsh_wrap_escapes("hello"), "hello");
    }

    #[test]
    fn zsh_wrap_single_escape() {
        let input = format!("{}text", fg(31));
        let wrapped = zsh_wrap_escapes(&input);
        assert_eq!(wrapped, "%{\x1b[38;5;31m%}text");
    }

    #[test]
    fn zsh_wrap_multiple_escapes() {
        let input = format!("{}hello{}world", fg(31), bg(236));
        let wrapped = zsh_wrap_escapes(&input);
        assert_eq!(wrapped, "%{\x1b[38;5;31m%}hello%{\x1b[48;5;236m%}world");
    }

    #[test]
    fn zsh_wrap_preserves_visible_text() {
        let input = format!("{} $ {}", fg(15), fg(9));
        let wrapped = zsh_wrap_escapes(&input);
        assert!(wrapped.contains(" $ "));
        assert!(wrapped.contains("%{"));
        assert!(wrapped.contains("%}"));
    }

    #[test]
    fn arrow_with_from_bg_includes_glyph() {
        let out = arrow(Some(237), 148);
        assert!(out.contains(fg(237)));
        assert!(out.contains(bg(148)));
        assert!(out.contains(ARROW));
    }

    #[test]
    fn arrow_without_from_bg_is_just_bg() {
        let out = arrow(None, 31);
        assert_eq!(out, bg(31));
        assert!(!out.contains(ARROW));
    }

    #[test]
    fn bash_wrap_single_escape() {
        let input = format!("{}text", fg(31));
        let wrapped = bash_wrap_escapes(&input);
        assert_eq!(wrapped, "\x01\x1b[38;5;31m\x02text");
    }

    #[test]
    fn bash_wrap_multiple_escapes() {
        let input = format!("{}hello{}world", fg(31), bg(236));
        let wrapped = bash_wrap_escapes(&input);
        assert_eq!(
            wrapped,
            "\x01\x1b[38;5;31m\x02hello\x01\x1b[48;5;236m\x02world"
        );
    }

    #[test]
    fn bash_wrap_no_escapes() {
        assert_eq!(bash_wrap_escapes("hello"), "hello");
    }

    #[test]
    fn wrap_for_shell_zsh_default() {
        let input = format!("{}text", fg(31));
        let wrapped = wrap_for_shell("zsh", &input);
        assert_eq!(wrapped, zsh_wrap_escapes(&input));
    }

    #[test]
    fn wrap_for_shell_bash() {
        let input = format!("{}text", fg(31));
        let wrapped = wrap_for_shell("bash", &input);
        assert_eq!(wrapped, bash_wrap_escapes(&input));
    }

    #[test]
    fn wrap_for_shell_fish_passthrough() {
        let input = format!("{}text", fg(31));
        let wrapped = wrap_for_shell("fish", &input);
        assert_eq!(wrapped, input);
    }

    #[test]
    fn wrap_for_shell_unknown_defaults_to_zsh() {
        let input = format!("{}text", fg(31));
        let wrapped = wrap_for_shell("unknown", &input);
        assert_eq!(wrapped, zsh_wrap_escapes(&input));
    }
}
