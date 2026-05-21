use std::fmt::Write;
use std::path::Path;

use crate::color::{THIN, arrow, fg};

fn truncate_dir(name: &str, max: usize) -> String {
    if max < 2 || name.chars().count() <= max {
        return name.to_string();
    }
    let truncated: String = name.chars().take(max - 1).collect();
    format!("{truncated}\u{2026}")
}

/// When `pwd` lives inside a git repo, strip the repo's parent directory
/// from the front so the repo name becomes the first visible component.
/// `~/src/chevron/src/segments` (workdir `~/src/chevron`) renders as
/// `chevron/src/segments` — the `~/src/` prefix is shared across many
/// repos and just adds noise.
///
/// Returns `None` if there's no workdir, the workdir has no parent, or
/// `pwd` isn't actually inside the workdir's parent (a defensive check
/// against caller bugs).
fn try_rebase_to_repo(pwd: &str, repo_workdir: Option<&Path>) -> Option<String> {
    let workdir = repo_workdir?;
    let workdir_parent = workdir.parent()?;
    let pwd_path = Path::new(pwd);
    let stripped = pwd_path.strip_prefix(workdir_parent).ok()?;
    let rendered = stripped.to_string_lossy().into_owned();
    if rendered.is_empty() {
        return None;
    }
    Some(rendered)
}

#[must_use]
pub fn render_with(
    home: &str,
    pwd: &str,
    max_dir_size: Option<usize>,
    from_bg: Option<u8>,
) -> (String, Option<u8>) {
    render_with_repo(home, pwd, max_dir_size, from_bg, None)
}

#[must_use]
pub fn render_with_repo(
    home: &str,
    pwd: &str,
    max_dir_size: Option<usize>,
    from_bg: Option<u8>,
    repo_workdir: Option<&Path>,
) -> (String, Option<u8>) {
    // Adaptive: inside a git repo, render relative to the repo's parent
    // dir so the repo name leads. Outside a repo (or when explicitly
    // disabled via CHEVRON_REPO_RELATIVE_PATH=0), fall through to the
    // home-collapse behaviour.
    let repo_rebase = if std::env::var("CHEVRON_REPO_RELATIVE_PATH").ok().as_deref() == Some("0") {
        None
    } else {
        try_rebase_to_repo(pwd, repo_workdir)
    };

    // Require an exact match or a `/` immediately after `home` so HOME=/home/user
    // does not match /home/user2/foo (which would render as "~2/foo").
    let path = if let Some(rebased) = repo_rebase {
        rebased
    } else if !home.is_empty()
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

/// Top-level entry for the `chevron path` subcommand. Discovers any git
/// repo at `pwd` so the repo-relative rendering kicks in even outside the
/// prompt path. The discover is ~50 µs cold; if it becomes a bottleneck
/// we could plumb the daemon's already-known workdir through the wire
/// protocol instead.
#[must_use]
pub fn render_aware(home: &str, pwd: &str, max_dir_size: Option<usize>) -> String {
    let workdir = git2::Repository::discover(pwd)
        .ok()
        .and_then(|r| r.workdir().and_then(|p| p.canonicalize().ok()));
    render_with_repo(home, pwd, max_dir_size, Some(238), workdir.as_deref()).0
}

#[cfg(test)]
mod tests {
    use super::{render, render_with, render_with_repo, truncate_dir, try_rebase_to_repo};
    use serial_test::serial;
    use std::path::Path;

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
        let out = render("/home/user", "/home/user/projects/chevron", None);
        assert!(out.contains('~'), "expected ~ for home subdir");
        assert!(out.contains("chevron"));
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

    // ── chevron-ir6: repo-relative path rendering ───────────────────────

    #[test]
    fn rebase_strips_workdir_parent_at_workdir_root() {
        let out = try_rebase_to_repo(
            "/Users/mim/src/chevron",
            Some(Path::new("/Users/mim/src/chevron")),
        );
        assert_eq!(out.as_deref(), Some("chevron"));
    }

    #[test]
    fn rebase_strips_workdir_parent_deep() {
        let out = try_rebase_to_repo(
            "/Users/mim/src/chevron/src/segments",
            Some(Path::new("/Users/mim/src/chevron")),
        );
        assert_eq!(out.as_deref(), Some("chevron/src/segments"));
    }

    #[test]
    fn rebase_returns_none_for_pwd_outside_workdir_parent() {
        // pwd is sibling to the repo, not under its parent → don't rebase.
        let out = try_rebase_to_repo("/var/log", Some(Path::new("/Users/mim/src/chevron")));
        assert!(out.is_none());
    }

    #[test]
    fn rebase_returns_none_when_no_workdir() {
        let out = try_rebase_to_repo("/anywhere", None);
        assert!(out.is_none());
    }

    #[test]
    fn rebase_returns_none_when_pwd_equals_workdir_parent() {
        // pwd is the parent directory itself — strip_prefix returns empty.
        // That's not a useful path to render; bail and let the caller fall
        // back to home-collapse.
        let out = try_rebase_to_repo("/Users/mim/src", Some(Path::new("/Users/mim/src/chevron")));
        assert!(out.is_none());
    }

    #[test]
    fn rebase_handles_workdir_at_filesystem_root() {
        // /repo's parent is /. strip_prefix("/")  on /repo/sub → "repo/sub".
        let out = try_rebase_to_repo("/repo/sub", Some(Path::new("/repo")));
        assert_eq!(out.as_deref(), Some("repo/sub"));
    }

    #[test]
    fn render_inside_repo_drops_home_prefix() {
        let out = render_with_repo(
            "/Users/mim",
            "/Users/mim/src/chevron/src/segments",
            None,
            Some(238),
            Some(Path::new("/Users/mim/src/chevron")),
        )
        .0;
        // Should NOT contain ~ — we used the repo-relative form instead.
        let visible: String = out
            .chars()
            .filter(|c| !c.is_ascii_control() && *c != '\u{1b}')
            .collect();
        assert!(
            !visible.contains('~'),
            "inside-repo render should not show ~: {visible}"
        );
        assert!(visible.contains("chevron"), "should show repo name");
        assert!(visible.contains("segments"), "should show deep dir");
        // ~/src should not appear as path segments — the whole point.
        assert!(
            !visible.contains(" mim "),
            "should not include $HOME path: {visible}"
        );
    }

    #[test]
    fn render_inside_repo_at_workdir_root() {
        let out = render_with_repo(
            "/Users/mim",
            "/Users/mim/src/chevron",
            None,
            Some(238),
            Some(Path::new("/Users/mim/src/chevron")),
        )
        .0;
        let visible: String = out
            .chars()
            .filter(|c| !c.is_ascii_control() && *c != '\u{1b}')
            .collect();
        assert!(visible.contains("chevron"));
        assert!(!visible.contains('~'), "{visible}");
    }

    #[test]
    fn render_outside_repo_keeps_home_collapse() {
        // No workdir → home-collapse path unchanged.
        let out = render_with_repo("/Users/mim", "/Users/mim/Documents", None, Some(238), None).0;
        let visible: String = out
            .chars()
            .filter(|c| !c.is_ascii_control() && *c != '\u{1b}')
            .collect();
        assert!(
            visible.contains('~'),
            "outside-repo should still home-collapse: {visible}"
        );
    }

    #[test]
    #[serial]
    fn opt_out_via_env_var_disables_rebase() {
        // SAFETY: test-only env mutation, serialised.
        unsafe { std::env::set_var("CHEVRON_REPO_RELATIVE_PATH", "0") };
        let out = render_with_repo(
            "/Users/mim",
            "/Users/mim/src/chevron/src",
            None,
            Some(238),
            Some(Path::new("/Users/mim/src/chevron")),
        )
        .0;
        unsafe { std::env::remove_var("CHEVRON_REPO_RELATIVE_PATH") };
        let visible: String = out
            .chars()
            .filter(|c| !c.is_ascii_control() && *c != '\u{1b}')
            .collect();
        // With the opt-out, we fall back to home-collapse → ~ appears.
        assert!(
            visible.contains('~'),
            "opt-out should restore home-collapse: {visible}"
        );
    }

    #[test]
    fn render_with_uses_from_bg() {
        let (out, end_bg) = render_with("/home/user", "/home/user/src", None, Some(240));
        assert!(
            out.contains(crate::color::fg(240)),
            "expected fg(240) in: {out}"
        );
        assert_eq!(end_bg, Some(237));
    }

    // ── Snapshot tests ──────────────────────────────────────────────────
    //
    // Lock down the visible ANSI output for representative inputs. Snapshots
    // live in src/segments/snapshots/. Regenerate with `cargo insta accept`
    // after intentional format changes.

    #[test]
    fn snap_home_dir() {
        insta::assert_snapshot!(render("/home/user", "/home/user", None));
    }

    #[test]
    fn snap_home_subdir() {
        insta::assert_snapshot!(render("/home/user", "/home/user/src/chevron", None));
    }

    #[test]
    fn snap_deep_path_truncated() {
        insta::assert_snapshot!(render("", "/a/b/c/d/e/f/g/h/i", None));
    }

    #[test]
    fn snap_non_home() {
        insta::assert_snapshot!(render("/home/user", "/var/log/system.log", None));
    }

    #[test]
    fn snap_root() {
        insta::assert_snapshot!(render("/home/user", "/", None));
    }

    #[test]
    fn snap_max_dir_size_truncates() {
        insta::assert_snapshot!(render("", "/home/very-long-directory-name/src", Some(10)));
    }

    // ── Property tests ──────────────────────────────────────────────────
    use proptest::prelude::*;

    /// Generate path-like strings: 1-6 ASCII-letter segments joined by `/`,
    /// optionally rooted at `/`.
    fn path_strategy() -> impl Strategy<Value = String> {
        (
            prop::bool::ANY,
            prop::collection::vec("[a-zA-Z][a-zA-Z0-9_-]{0,8}", 1..=6),
        )
            .prop_map(|(rooted, parts)| {
                let joined = parts.join("/");
                if rooted { format!("/{joined}") } else { joined }
            })
    }

    proptest! {
        // Property: render never panics for any home/pwd combo.
        #[test]
        fn render_never_panics(home in "[^\\x00]{0,40}", pwd in path_strategy()) {
            let _ = render(&home, &pwd, None);
            let _ = render(&home, &pwd, Some(20));
        }

        // Property: if home is empty, output contains no `~` collapse.
        #[test]
        fn empty_home_never_produces_tilde(pwd in path_strategy()) {
            let out = render("", &pwd, None);
            // Strip ANSI to focus on visible text.
            let visible: String = out
                .chars()
                .filter(|c| !c.is_ascii_control())
                .collect();
            prop_assert!(!visible.contains('~'), "got ~ for empty home with pwd={pwd}: {out}");
        }

        // Property: if pwd does not start with home or is not `/`-bounded after,
        // the rendered output does not collapse to ~. This is the property the
        // /home/user2 bug violated.
        #[test]
        fn unrelated_pwd_never_collapses(
            home in "/[a-z][a-z0-9]{0,8}/[a-z][a-z0-9]{0,8}",
            pwd in path_strategy(),
        ) {
            // Construct a pwd that is intentionally NOT a /-bounded child of home.
            // Append a non-/ character right after home to break boundary.
            let confusing_pwd = format!("{home}suffix/{pwd}");
            let out = render(&home, &confusing_pwd, None);
            // Strip ANSI for visible-text inspection.
            let visible: String = out
                .chars()
                .filter(|c| !c.is_ascii_control() && *c != '\u{1b}')
                .collect();
            // The visible output should mention "suffix" (the unrelated tail)
            // and must not collapse the whole prefix into `~`.
            prop_assert!(
                !visible.starts_with(' ')  // ANSI-prefixed renders may start with space
                    || !visible.contains('~')
                    || visible.contains("suffix"),
                "home={home} pwd={confusing_pwd} -> {out}"
            );
        }

        // Property: truncate_dir produces a string of length <= max chars (where
        // max >= 2, since 2 covers `<one char>…`).
        #[test]
        fn truncate_dir_respects_max(name in "[a-zA-Z0-9_-]{1,30}", max in 2usize..15) {
            let out = truncate_dir(&name, max);
            // We count chars, not bytes, because the ellipsis is multi-byte.
            let char_count = out.chars().count();
            prop_assert!(
                char_count <= max,
                "max={max} produced {char_count}-char output {out:?} from {name:?}"
            );
        }

        // Property: a name shorter than or equal to max is unchanged.
        #[test]
        fn truncate_dir_short_unchanged(name in "[a-zA-Z0-9_-]{1,8}", max in 8usize..20) {
            let out = truncate_dir(&name, max);
            prop_assert_eq!(out, name);
        }
    }
}
