use std::fmt::Write;

use crate::color::{BOLD, UNBOLD, arrow, fg};

const MY_BG: u8 = 240;
const MY_FG: u8 = 250;
const ROOT_FG: u8 = 9;

#[must_use]
pub fn render_with(from_bg: Option<u8>) -> (String, Option<u8>) {
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty() {
        return (String::new(), from_bg);
    }

    let is_root = user == "root";
    let mut out = String::with_capacity(64);

    if is_root {
        let _ = write!(
            out,
            "{} {BOLD}{}{user}{UNBOLD} ",
            arrow(from_bg, MY_BG),
            fg(ROOT_FG)
        );
    } else {
        let _ = write!(out, "{} {}{user} ", arrow(from_bg, MY_BG), fg(MY_FG));
    }

    (out, Some(MY_BG))
}

#[cfg(test)]
mod tests {
    use super::render_with;
    use crate::color::{ARROW, bg, fg};
    use serial_test::serial;

    #[test]
    #[serial]
    fn empty_when_user_unset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("USER") };
        let (out, end_bg) = render_with(None);
        assert_eq!(out, "");
        assert_eq!(end_bg, None);
        unsafe { std::env::set_var("USER", "testuser") };
    }

    #[test]
    #[serial]
    fn renders_username() {
        // SAFETY: test-only
        unsafe { std::env::set_var("USER", "alice") };
        let (out, end_bg) = render_with(None);
        assert!(out.contains("alice"), "expected username in: {out}");
        assert!(out.contains(bg(240)), "expected bg(240) in: {out}");
        assert!(out.contains(fg(250)), "expected fg(250) in: {out}");
        assert_eq!(end_bg, Some(240));
        assert!(
            !out.contains(ARROW),
            "should not have arrow when first segment"
        );
    }

    #[test]
    #[serial]
    fn draws_arrow_when_not_first() {
        // SAFETY: test-only
        unsafe { std::env::set_var("USER", "alice") };
        let (out, _) = render_with(Some(238));
        assert!(out.contains(ARROW), "expected arrow in: {out}");
        assert!(
            out.contains(fg(238)),
            "expected fg(238) for arrow in: {out}"
        );
    }

    #[test]
    #[serial]
    fn root_user_bold_red() {
        // SAFETY: test-only
        unsafe { std::env::set_var("USER", "root") };
        let (out, end_bg) = render_with(None);
        assert!(out.contains("root"), "expected root in: {out}");
        assert!(out.contains(fg(9)), "expected red fg(9) in: {out}");
        assert!(out.contains("\x1b[1m"), "expected bold in: {out}");
        assert!(out.contains("\x1b[22m"), "expected unbold in: {out}");
        assert_eq!(end_bg, Some(240));
        unsafe { std::env::set_var("USER", "testuser") };
    }
}
