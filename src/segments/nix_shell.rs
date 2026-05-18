use std::fmt::Write;

use crate::color::{arrow, fg};

#[must_use]
pub fn render_with(from_bg: Option<u8>) -> (String, Option<u8>) {
    let in_nix = std::env::var("IN_NIX_SHELL").unwrap_or_default();
    if in_nix.is_empty() {
        return (String::new(), from_bg);
    }

    let mut out = String::with_capacity(128);
    let _ = write!(
        out,
        "{} {}❄ nix {}",
        arrow(from_bg, 68),
        fg(15),
        arrow(Some(68), 237),
    );
    (out, Some(237))
}

#[must_use]
pub fn render() -> String {
    render_with(Some(237)).0
}

#[cfg(test)]
mod tests {
    use super::{render, render_with};
    use crate::color::fg;
    use serial_test::serial;

    #[test]
    #[serial]
    fn empty_when_unset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };
        assert_eq!(render(), "");
    }

    #[test]
    #[serial]
    fn renders_segment_when_set() {
        // SAFETY: test-only
        unsafe { std::env::set_var("IN_NIX_SHELL", "impure") };
        let out = render();
        assert!(out.contains("nix"), "expected 'nix' in: {out}");
        assert!(out.contains('❄'), "expected snowflake in: {out}");
        unsafe { std::env::remove_var("IN_NIX_SHELL") };
    }

    #[test]
    #[serial]
    fn render_with_passthrough_when_unset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };
        let (out, end_bg) = render_with(Some(238));
        assert_eq!(out, "");
        assert_eq!(end_bg, Some(238));
    }

    #[test]
    #[serial]
    fn render_with_uses_from_bg() {
        // SAFETY: test-only
        unsafe { std::env::set_var("IN_NIX_SHELL", "impure") };
        let (out, end_bg) = render_with(Some(238));
        assert!(out.contains(fg(238)), "expected fg(238) in: {out}");
        assert_eq!(end_bg, Some(237));
        unsafe { std::env::remove_var("IN_NIX_SHELL") };
    }
}
