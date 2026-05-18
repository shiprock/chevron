use crate::color::fg;

#[must_use]
pub fn render_with(success: bool, from_bg: Option<u8>) -> (String, Option<u8>) {
    let color = if success { 15 } else { 9 };
    (format!("{} $ ", fg(color)), from_bg)
}

#[cfg(test)]
mod tests {
    use super::render_with;
    use crate::color::fg;

    #[test]
    fn success_white() {
        let (out, _) = render_with(true, Some(236));
        assert!(out.contains(fg(15)), "expected fg(15) in: {out}");
        assert!(out.contains('$'));
    }

    #[test]
    fn error_red() {
        let (out, _) = render_with(false, Some(236));
        assert!(out.contains(fg(9)), "expected fg(9) in: {out}");
    }
}
