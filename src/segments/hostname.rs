use std::fmt::Write;

use crate::color::{arrow, fg};

const MY_BG: u8 = 238;
const MY_FG: u8 = 250;
const SSH_ICON: &str = "\u{F817}";

fn get_hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is a writable 256-byte stack array; gethostname writes at
    // most `buf.len()` bytes and we only read `buf` after checking ret == 0.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if ret != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(&buf[..len]).into_owned(),
    }
}

#[must_use]
pub fn render_with(from_bg: Option<u8>) -> (String, Option<u8>) {
    let hostname = get_hostname();
    if hostname.is_empty() {
        return (String::new(), from_bg);
    }

    let is_ssh = std::env::var("SSH_CONNECTION")
        .ok()
        .is_some_and(|v| !v.is_empty());

    let mut out = String::with_capacity(128);

    if is_ssh {
        let _ = write!(
            out,
            "{} {}{SSH_ICON} {hostname} ",
            arrow(from_bg, MY_BG),
            fg(MY_FG)
        );
    } else {
        let _ = write!(out, "{} {}{hostname} ", arrow(from_bg, MY_BG), fg(MY_FG));
    }

    (out, Some(MY_BG))
}

#[cfg(test)]
mod tests {
    use super::{get_hostname, render_with};
    use crate::color::{ARROW, bg, fg};
    use serial_test::serial;

    #[test]
    fn gets_hostname() {
        let name = get_hostname();
        assert!(!name.is_empty(), "hostname should not be empty");
    }

    #[test]
    #[serial]
    fn renders_hostname_segment() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("SSH_CONNECTION") };
        let (out, end_bg) = render_with(Some(240));
        assert!(out.contains(bg(238)), "expected bg(238) in: {out}");
        assert!(out.contains(fg(240)), "expected arrow fg(240) in: {out}");
        assert!(out.contains(ARROW), "expected arrow in: {out}");
        assert_eq!(end_bg, Some(238));
    }

    #[test]
    #[serial]
    fn ssh_shows_icon() {
        // SAFETY: test-only
        unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 22 5.6.7.8 54321") };
        let (out, _) = render_with(Some(240));
        assert!(out.contains('\u{F817}'), "expected SSH icon in: {out}");
        unsafe { std::env::remove_var("SSH_CONNECTION") };
    }

    #[test]
    #[serial]
    fn no_ssh_no_icon() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("SSH_CONNECTION") };
        let (out, _) = render_with(Some(240));
        assert!(!out.contains('\u{F817}'), "should not have SSH icon: {out}");
    }

    #[test]
    #[serial]
    fn first_segment_no_arrow() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("SSH_CONNECTION") };
        let (out, end_bg) = render_with(None);
        assert!(
            !out.contains(ARROW),
            "should not have arrow when first segment"
        );
        assert!(out.contains(bg(238)), "expected bg(238) in: {out}");
        assert_eq!(end_bg, Some(238));
    }
}
