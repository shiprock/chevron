use std::fmt::Write;

use crate::color::{arrow, fg};

#[must_use]
pub fn render_with(from_bg: Option<u8>) -> (String, Option<u8>) {
    let profile = std::env::var("AWS_PROFILE").unwrap_or_default();
    if profile.is_empty() {
        return (String::new(), from_bg);
    }

    let mut out = String::with_capacity(128);
    let _ = write!(out, "{} {} {profile} ", arrow(from_bg, 208), fg(0));
    (out, Some(208))
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
        unsafe { std::env::remove_var("AWS_PROFILE") };
        assert_eq!(render(), "");
    }

    #[test]
    #[serial]
    fn renders_profile_when_set() {
        // SAFETY: test-only
        unsafe { std::env::set_var("AWS_PROFILE", "prod-admin") };
        let out = render();
        assert!(
            out.contains("prod-admin"),
            "expected profile name in: {out}"
        );
        unsafe { std::env::remove_var("AWS_PROFILE") };
    }

    #[test]
    #[serial]
    fn render_with_passthrough_when_unset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("AWS_PROFILE") };
        let (out, end_bg) = render_with(Some(238));
        assert_eq!(out, "");
        assert_eq!(end_bg, Some(238));
    }

    #[test]
    #[serial]
    fn render_with_uses_from_bg() {
        // SAFETY: test-only
        unsafe { std::env::set_var("AWS_PROFILE", "staging") };
        let (out, end_bg) = render_with(Some(238));
        assert!(out.contains(fg(238)), "expected fg(238) in: {out}");
        assert_eq!(end_bg, Some(208));
        unsafe { std::env::remove_var("AWS_PROFILE") };
    }
}
