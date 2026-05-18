use std::fmt::Write;

use crate::color::{THIN, arrow, fg};

fn truncate_dir(name: &str, max: usize) -> String {
    if max < 2 || name.chars().count() <= max {
        return name.to_string();
    }
    let truncated: String = name.chars().take(max - 1).collect();
    format!("{truncated}\u{2026}")
}

#[must_use]
pub fn render_with(
    home: &str,
    pwd: &str,
    max_dir_size: Option<usize>,
    from_bg: Option<u8>,
) -> (String, Option<u8>) {
    // Require an exact match or a `/` immediately after `home` so HOME=/home/user
    // does not match /home/user2/foo (which would render as "~2/foo").
    let path = if !home.is_empty()
        && pwd.starts_with(home)
        && (pwd.len() == home.len() || pwd.as_bytes().get(home.len()) == Some(&b'/'))
    {
        format!("~{}", &pwd[home.len()..])
    } else {
        pwd.to_string()
    };

    let raw_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let raw_parts = if raw_parts.is_empty() {
        vec!["/"]
    } else {
        raw_parts
    };

    let n = raw_parts.len();
    let raw_parts = if n > 5 {
        [&["\u{2026}"][..], &raw_parts[n - 4..]].concat()
    } else {
        raw_parts
    };

    let parts: Vec<String> = raw_parts
        .iter()
        .map(|p| {
            if let Some(max) = max_dir_size {
                truncate_dir(p, max)
            } else {
                (*p).to_string()
            }
        })
        .collect();

    let n = parts.len();
    let mut out = String::with_capacity(256);

    if n <= 1 {
        let _ = write!(
            out,
            "{} {}{} {}",
            arrow(from_bg, 31),
            fg(15),
            parts.first().map_or("", String::as_str),
            arrow(Some(31), 237),
        );
    } else {
        let _ = write!(
            out,
            "{} {}{} {}",
            arrow(from_bg, 31),
            fg(15),
            parts[0],
            arrow(Some(31), 237),
        );

        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate().skip(1) {
            if i > 1 {
                let _ = write!(out, " {}{THIN}", fg(244));
            }
            let color = if i == last { 254 } else { 250 };
            let _ = write!(out, " {}{part}", fg(color));
        }
        let _ = write!(out, " ");
    }

    (out, Some(237))
}

#[must_use]
pub fn render(home: &str, pwd: &str, max_dir_size: Option<usize>) -> String {
    render_with(home, pwd, max_dir_size, Some(238)).0
}

#[cfg(test)]
mod tests {
    use super::{render, render_with, truncate_dir};

    #[test]
    fn home_shows_tilde() {
        let out = render("/home/user", "/home/user", None);
        assert!(out.contains('~'), "expected ~ in: {out}");
        assert!(!out.contains("/home"), "should not contain raw home path");
    }

    #[test]
    fn root_shows_slash() {
        let out = render("/home/user", "/", None);
        assert!(out.contains('/'), "expected / in: {out}");
    }

    #[test]
    fn deep_truncation() {
        let out = render("", "/a/b/c/d/e/f/g", None);
        assert!(out.contains('\u{2026}'), "expected ellipsis in: {out}");
        assert!(out.contains('g'), "expected last component");
    }

    #[test]
    fn five_components_no_truncation() {
        let out = render("", "/a/b/c/d/e", None);
        assert!(
            !out.contains('\u{2026}'),
            "should not truncate 5 components"
        );
        assert!(out.contains('a'));
        assert!(out.contains('e'));
    }

    #[test]
    fn non_home_no_tilde() {
        let out = render("/home/user", "/var/log", None);
        assert!(!out.contains('~'), "should not contain ~ for non-home path");
        assert!(out.contains("var"));
        assert!(out.contains("log"));
    }

    #[test]
    fn single_component() {
        let out = render("/home/user", "/tmp", None);
        assert!(out.contains("tmp"));
    }

    #[test]
    fn home_subdir_shows_tilde() {
        let out = render("/home/user", "/home/user/projects/plx", None);
        assert!(out.contains('~'), "expected ~ for home subdir");
        assert!(out.contains("plx"));
    }

    #[test]
    fn similar_prefix_does_not_collapse_to_tilde() {
        // /home/user is a prefix of /home/user2/foo but they're different users.
        let out = render("/home/user", "/home/user2/foo", None);
        assert!(
            !out.contains('~'),
            "should not show ~ when home is only a string prefix: {out}"
        );
        assert!(out.contains("user2"), "expected raw user2 in: {out}");
    }

    #[test]
    fn truncate_dir_long_name() {
        assert_eq!(
            truncate_dir("very-long-directory-name", 10),
            "very-long\u{2026}"
        );
    }

    #[test]
    fn truncate_dir_short_name_unchanged() {
        assert_eq!(truncate_dir("short", 10), "short");
    }

    #[test]
    fn truncate_dir_exact_length() {
        assert_eq!(truncate_dir("exactly10!", 10), "exactly10!");
    }

    #[test]
    fn max_dir_size_truncates_long_parts() {
        let out = render("", "/home/very-long-directory-name/src", Some(10));
        assert!(
            out.contains("very-long\u{2026}"),
            "expected truncated dir in: {out}"
        );
        assert!(out.contains("src"), "short names should be untouched");
    }

    #[test]
    fn max_dir_size_none_preserves_long_names() {
        let out = render("", "/home/very-long-directory-name/src", None);
        assert!(
            out.contains("very-long-directory-name"),
            "should preserve full name: {out}"
        );
    }

    #[test]
    fn render_with_uses_from_bg() {
        let (out, end_bg) = render_with("/home/user", "/home/user/src", None, Some(240));
        assert!(
            out.contains(&crate::color::fg(240)),
            "expected fg(240) in: {out}"
        );
        assert_eq!(end_bg, Some(237));
    }
}
