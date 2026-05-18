use git2::Repository;

use crate::color::{BRANCH_ICON, PENCIL_ICON};
use crate::segments::git::RepoStatus;

/// Render tmux title from a pre-computed [`RepoStatus`], avoiding redundant
/// repo discovery. Used by the prompt path which has already computed status
/// for the git segment and stashes it in `PromptContext::repo_status`.
#[must_use]
pub fn render_from_status(home: &str, pwd: &str, status: Option<&RepoStatus>) -> String {
    if pwd == home {
        return "\u{1F3E0} ~".to_string();
    }

    let dir_name = std::path::Path::new(pwd)
        .file_name()
        .map_or_else(|| pwd.to_string(), |n| n.to_string_lossy().to_string());

    let Some(s) = status else {
        return format!("\u{1F4C1} {dir_name}");
    };

    if s.is_dirty() {
        format!(
            "#[fg=colour174]{BRANCH_ICON}#[default] {} {} #[fg=colour245]{PENCIL_ICON}#[default]",
            s.repo_name, s.branch
        )
    } else {
        format!(
            "#[fg=colour117]{BRANCH_ICON}#[default] {} {}",
            s.repo_name, s.branch
        )
    }
}

/// Standalone entry for the `plx tmux-title` subcommand. Discovers the repo
/// itself and computes a full `RepoStatus`. Shares its compute path with the
/// prompt git segment so the title can't disagree with the prompt's idea of
/// dirtiness.
#[must_use]
pub fn render(home: &str, pwd: &str) -> String {
    if pwd == home {
        return "\u{1F3E0} ~".to_string();
    }

    let Ok(mut repo) = Repository::discover(pwd) else {
        let dir_name = std::path::Path::new(pwd)
            .file_name()
            .map_or_else(|| pwd.to_string(), |n| n.to_string_lossy().to_string());
        return format!("\u{1F4C1} {dir_name}");
    };

    let status = RepoStatus::compute(&mut repo);
    render_from_status(home, pwd, Some(&status))
}

#[cfg(test)]
mod tests {
    use super::{render, render_from_status};
    use crate::color::{BRANCH_ICON, PENCIL_ICON};
    use crate::segments::git::RepoStatus;
    use crate::segments::testutil::init_repo;
    use std::fs;
    use tempfile::TempDir;

    fn clean_status(repo_name: &str, branch: &str) -> RepoStatus {
        RepoStatus {
            repo_name: repo_name.to_string(),
            branch: branch.to_string(),
            detached: false,
            state: None,
            staged: 0,
            modified: 0,
            untracked: 0,
            conflicted: 0,
            ahead: 0,
            behind: 0,
            stashed: 0,
        }
    }

    // ── Snapshot tests ──────────────────────────────────────────────────
    // Lock down the tmux title format for representative scenarios. Snapshots
    // live in src/segments/snapshots/. Regenerate with `cargo insta accept`
    // after intentional format changes.

    #[test]
    fn snap_clean_repo() {
        insta::assert_snapshot!(render_from_status(
            "/home/user",
            "/home/user/src/plx",
            Some(&clean_status("plx", "master"))
        ));
    }

    #[test]
    fn snap_dirty_repo() {
        let s = RepoStatus {
            modified: 1,
            ..clean_status("plx", "feature/refactor")
        };
        insta::assert_snapshot!(render_from_status(
            "/home/user",
            "/home/user/src/plx",
            Some(&s)
        ));
    }

    #[test]
    fn snap_home_no_repo() {
        insta::assert_snapshot!(render_from_status("/home/user", "/home/user", None));
    }

    #[test]
    fn snap_non_repo_directory() {
        insta::assert_snapshot!(render_from_status("/home/user", "/tmp/scratch", None));
    }

    #[test]
    fn home_directory() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_string_lossy().to_string();
        let out = render(&home, &home);
        assert!(out.contains('\u{1F3E0}'), "expected house emoji");
        assert!(out.contains('~'));
    }

    #[test]
    fn non_repo_directory() {
        let tmp = TempDir::new().unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        let out = render("/nonexistent", &pwd);
        assert!(out.contains('\u{1F4C1}'), "expected folder emoji");
    }

    #[test]
    fn clean_repo() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let pwd = tmp.path().to_string_lossy().to_string();

        let out = render("/nonexistent", &pwd);
        assert!(
            out.contains("#[fg=colour117]"),
            "expected blue branch in: {out}"
        );
        assert!(out.contains(BRANCH_ICON));
        assert!(
            !out.contains(PENCIL_ICON),
            "clean repo should not have pencil"
        );
    }

    #[test]
    fn dirty_repo() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();

        let out = render("/nonexistent", &pwd);
        assert!(
            out.contains("#[fg=colour174]"),
            "expected pink branch in: {out}"
        );
        assert!(out.contains(PENCIL_ICON), "dirty repo should have pencil");
    }

    // ── render_from_status tests ────────────────────────────────────────

    #[test]
    fn render_from_status_home_returns_house() {
        let home = "/home/user";
        let out = render_from_status(home, home, None);
        assert!(out.contains('\u{1F3E0}'), "expected house emoji");
        assert!(out.contains('~'));
    }

    #[test]
    fn render_from_status_no_status_returns_folder() {
        let tmp = TempDir::new().unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        let out = render_from_status("/nonexistent", &pwd, None);
        assert!(out.contains('\u{1F4C1}'), "expected folder emoji in: {out}");
    }

    #[test]
    fn render_from_status_clean_repo_is_blue() {
        let s = clean_status("myrepo", "main");
        let out = render_from_status("/home/user", "/home/user/src", Some(&s));
        assert!(
            out.contains("#[fg=colour117]"),
            "expected blue branch in: {out}"
        );
        assert!(out.contains("myrepo"), "expected repo name in: {out}");
        assert!(out.contains("main"), "expected branch in: {out}");
        assert!(
            !out.contains(PENCIL_ICON),
            "clean repo should not have pencil icon"
        );
    }

    #[test]
    fn render_from_status_dirty_repo_is_pink_with_pencil() {
        let s = RepoStatus {
            modified: 1,
            ..clean_status("myrepo", "feature")
        };
        let out = render_from_status("/home/user", "/home/user/src", Some(&s));
        assert!(
            out.contains("#[fg=colour174]"),
            "expected pink branch in: {out}"
        );
        assert!(
            out.contains(PENCIL_ICON),
            "dirty repo should have pencil icon"
        );
        assert!(out.contains("feature"), "expected branch name in: {out}");
        assert!(out.contains(BRANCH_ICON), "expected branch icon in: {out}");
    }
}
