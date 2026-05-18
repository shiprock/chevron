use crate::color::{ARROW, arrow, bg, fg};

#[must_use]
pub fn render_with(exit_status: i32, from_bg: Option<u8>) -> (String, Option<u8>) {
    if exit_status == 0 {
        return (String::new(), from_bg);
    }

    let out = format!(
        "{} {}{exit_status} {}{}{}",
        arrow(from_bg, 196),
        fg(15),
        fg(196),
        bg(236),
        ARROW,
    );

    (out, Some(236))
}

#[cfg(test)]
mod tests {
    use super::render_with;
    use crate::color::{ARROW, bg, fg};

    #[test]
    fn zero_exit_is_empty() {
        let (out, end_bg) = render_with(0, Some(236));
        assert!(out.is_empty());
        assert_eq!(end_bg, Some(236));
    }

    #[test]
    fn nonzero_shows_badge() {
        let (out, end_bg) = render_with(1, Some(236));
        assert!(out.contains(bg(196)), "expected red bg in: {out}");
        assert!(out.contains('1'), "expected exit code in: {out}");
        assert!(out.contains(ARROW));
        assert!(out.contains(fg(196)));
        assert_eq!(end_bg, Some(236));
    }
}
