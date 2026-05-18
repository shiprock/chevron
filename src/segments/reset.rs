use crate::color::{ARROW, RST, fg};

#[must_use]
pub fn render_final(from_bg: Option<u8>) -> String {
    if let Some(color) = from_bg {
        format!("{RST}{}{ARROW}{RST}", fg(color))
    } else {
        RST.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::render_final;
    use crate::color::{ARROW, RST, fg};

    #[test]
    fn with_bg() {
        let out = render_final(Some(236));
        assert!(out.starts_with(RST));
        assert!(out.contains(fg(236)));
        assert!(out.contains(ARROW));
        assert!(out.ends_with(RST));
    }

    #[test]
    fn no_bg() {
        let out = render_final(None);
        assert_eq!(out, RST);
    }
}
