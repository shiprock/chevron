use std::fmt::Write;

use git2::{AttrCheckFlags, Repository, StatusOptions, StatusShow};

use crate::color::{ARROW, BRANCH_ICON, RST, arrow, bg, fg};

/// In-flight repository state (rebase, merge, cherry-pick, bisect).
/// Rendered as a yellow chip immediately after the branch when dirty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    Rebasing,
    Merging,
    CherryPick,
    Bisect,
}

impl OpState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Rebasing => "REBASING",
            Self::Merging => "MERGING",
            Self::CherryPick => "CHERRY",
            Self::Bisect => "BISECT",
        }
    }
}

/// Complete snapshot of the data the git segment renders: repo identity,
/// current branch (named or detached SHA), in-flight git operation, and
/// staged / modified / untracked / conflicted / ahead / behind / stashed
/// counts. Produced in a single libgit2 pass by [`RepoStatus::compute`].
///
/// The renderer [`render_segment`] is pure over this struct — no I/O at
/// render time. This is the data type the chevrond daemon caches and
/// serves in later phases; the inline-fallback path and daemon path both
/// produce `RepoStatus` from identical code, so they cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoStatus {
    pub repo_name: String,
    /// Either the branch shorthand ("master", "feature/foo"), the 7-char
    /// detached HEAD SHA, or the literal "HEAD" if neither is resolvable.
    pub branch: String,
    pub detached: bool,
    pub state: Option<OpState>,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicted: u32,
    pub ahead: u32,
    pub behind: u32,
    pub stashed: u32,
}

impl RepoStatus {
    /// Single libgit2 pass collecting everything the git segment needs.
    /// `&mut` is required because `stash_foreach` does.
    #[must_use]
    pub fn compute(repo: &mut Repository) -> Self {
        let repo_name = repo_short_name(repo);
        let (branch, detached) = branch_info(repo);
        let state = op_state(repo);
        let (staged, modified, untracked, conflicted) = status_counts(repo);
        let (ahead, behind) = ahead_behind(repo);
        let stashed = stash_count(repo);
        Self {
            repo_name,
            branch,
            detached,
            state,
            staged,
            modified,
            untracked,
            conflicted,
            ahead,
            behind,
            stashed,
        }
    }

    /// True iff the segment should render in dirty (pink) mode. Stashes alone
    /// don't flip dirty — a clean repo with stashes shows green with a stash
    /// indicator chip.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.staged + self.modified + self.untracked + self.conflicted + self.ahead + self.behind
            > 0
            || self.state.is_some()
    }
}

fn repo_short_name(repo: &Repository) -> String {
    repo.workdir()
        .and_then(|p| p.file_name())
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

fn branch_info(repo: &Repository) -> (String, bool) {
    let detached = repo.head_detached().unwrap_or(false);
    let branch = if detached {
        repo.head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map_or_else(
                || "HEAD".to_string(),
                |c| c.id().to_string()[..7].to_string(),
            )
    } else {
        repo.head()
            .ok()
            .and_then(|h| h.shorthand().ok().map(str::to_string))
            .unwrap_or_else(|| "HEAD".to_string())
    };
    (branch, detached)
}

fn op_state(repo: &Repository) -> Option<OpState> {
    match repo.state() {
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive
        | git2::RepositoryState::RebaseMerge => Some(OpState::Rebasing),
        git2::RepositoryState::Merge => Some(OpState::Merging),
        git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
            Some(OpState::CherryPick)
        }
        git2::RepositoryState::Bisect => Some(OpState::Bisect),
        _ => None,
    }
}

fn status_counts(repo: &Repository) -> (u32, u32, u32, u32) {
    let mut staged = 0u32;
    let mut modified = 0u32;
    let mut untracked = 0u32;
    let mut conflicted = 0u32;

    let mut opts = StatusOptions::new();
    opts.show(StatusShow::IndexAndWorkdir);
    opts.include_untracked(true);

    // Hoisting: only invoke libgit2's per-path attribute lookup if the repo
    // actually has a .gitattributes file. Skips one get_attr() call per
    // modified file in the common case (no filter drivers).
    let check_filter_attrs = repo_has_attrs(repo);

    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return (0, 0, 0, 0);
    };

    for entry in statuses.iter() {
        let s = entry.status();
        if s.is_conflicted() {
            conflicted += 1;
            continue;
        }
        let is_staged = s.is_index_new()
            || s.is_index_modified()
            || s.is_index_deleted()
            || s.is_index_renamed()
            || s.is_index_typechange();
        if is_staged {
            staged += 1;
        }
        if (s.is_wt_modified() || s.is_wt_deleted() || s.is_wt_typechange())
            && !(check_filter_attrs && has_filter_attr(repo, entry.path().ok()))
        {
            modified += 1;
        }
        if s.is_wt_new() {
            untracked += 1;
        }
    }

    (staged, modified, untracked, conflicted)
}

fn ahead_behind(repo: &Repository) -> (u32, u32) {
    let Ok(head) = repo.head() else {
        return (0, 0);
    };

    let Some(local_oid) = head.target() else {
        return (0, 0);
    };

    let Ok(branch_name) = head.shorthand() else {
        return (0, 0);
    };

    let Ok(branch) = repo.find_branch(branch_name, git2::BranchType::Local) else {
        return (0, 0);
    };

    let Ok(upstream) = branch.upstream() else {
        return (0, 0);
    };

    let Some(upstream_oid) = upstream.get().target() else {
        return (0, 0);
    };

    repo.graph_ahead_behind(local_oid, upstream_oid)
        .map_or((0, 0), |(a, b)| {
            (
                u32::try_from(a).unwrap_or(u32::MAX),
                u32::try_from(b).unwrap_or(u32::MAX),
            )
        })
}

fn stash_count(repo: &mut Repository) -> u32 {
    let mut stashed = 0u32;
    let _ = repo.stash_foreach(|_, _, _| {
        stashed += 1;
        true
    });
    stashed
}

/// Cheap check: does this repo plausibly have any `.gitattributes` rules?
/// Used to short-circuit per-file `has_filter_attr` calls in the common case
/// where no filter drivers are configured.
fn repo_has_attrs(repo: &Repository) -> bool {
    let workdir_attrs = repo
        .workdir()
        .is_some_and(|w| w.join(".gitattributes").exists());
    let info_attrs = repo.path().join("info").join("attributes").exists();
    workdir_attrs || info_attrs
}

/// Returns true if the file has a `filter` gitattribute set (e.g. `filter=crypt`).
/// libgit2 does not run smudge/clean filters when comparing working tree content
/// to the index, so filtered files can appear falsely modified.
fn has_filter_attr(repo: &Repository, path: Option<&str>) -> bool {
    let Some(path) = path else { return false };
    repo.get_attr(
        std::path::Path::new(path),
        "filter",
        AttrCheckFlags::default(),
    )
    .is_ok_and(|v| v.is_some())
}

/// Pure renderer over a pre-computed `RepoStatus`. No I/O. `None` produces
/// the closing arrow appropriate for "not a repo" so the previous segment
/// terminates cleanly.
#[must_use]
pub fn render_segment(status: Option<&RepoStatus>, from_bg: Option<u8>) -> (String, Option<u8>) {
    let Some(s) = status else {
        return (arrow(from_bg, 236), Some(236));
    };

    let mut out = String::with_capacity(512);

    if s.is_dirty() {
        // Pink bar: arrow from previous(usually path 237) to 161
        let _ = write!(
            out,
            "{} {}{BRANCH_ICON} {} ",
            arrow(from_bg, 161),
            fg(15),
            s.branch,
        );
        let mut prev: u8 = 161;

        // In-flight op state (rebase/merge/cherry/bisect) — yellow chip
        if let Some(state) = s.state {
            let _ = write!(
                out,
                "{}{}{ARROW} {}{} ",
                fg(prev),
                bg(220),
                fg(0),
                state.label(),
            );
            prev = 220;
        }

        // Status indicator chips (kept in the existing visual order)
        let mut segs: Vec<(u8, String)> = Vec::new();
        if s.ahead > 0 {
            segs.push((240, format!("{}\u{2B06}", s.ahead)));
        }
        if s.behind > 0 {
            segs.push((240, format!("{}\u{2B07}", s.behind)));
        }
        if s.staged > 0 {
            segs.push((22, format!("{}\u{2714}", s.staged)));
        }
        if s.modified > 0 {
            segs.push((130, format!("{}\u{270E}", s.modified)));
        }
        if s.untracked > 0 {
            segs.push((52, format!("{}+", s.untracked)));
        }
        if s.conflicted > 0 {
            segs.push((9, format!("{}\u{273C}", s.conflicted)));
        }
        if s.stashed > 0 {
            segs.push((20, format!("{}\u{2691}", s.stashed)));
        }

        for (seg_bg, seg_text) in &segs {
            let _ = write!(
                out,
                "{}{}{ARROW} {}{seg_text} ",
                fg(prev),
                bg(*seg_bg),
                fg(15),
            );
            prev = *seg_bg;
        }

        // Final arrow to terminal bg (236)
        let _ = write!(out, "{}{}{ARROW}", fg(prev), bg(236));
    } else {
        // Green bar (clean). Stash indicator shown as a chip but doesn't flip
        // the bar to dirty.
        if s.stashed > 0 {
            let _ = write!(
                out,
                "{} {}{BRANCH_ICON} {} {}{}{ARROW} {}{}\u{2691} {}{}{ARROW}",
                arrow(from_bg, 148),
                fg(0),
                s.branch,
                fg(148),
                bg(20),
                fg(15),
                s.stashed,
                fg(20),
                bg(236),
            );
        } else {
            let _ = write!(
                out,
                "{} {}{BRANCH_ICON} {} {}{}{ARROW}",
                arrow(from_bg, 148),
                fg(0),
                s.branch,
                fg(148),
                bg(236),
            );
        }
    }

    (out, Some(236))
}

/// Top-level entry for the `chevron git` subcommand. Goes through
/// [`crate::daemon::status_for_cwd`] so the daemon-cached fast path is used
/// when available; falls back to inline `Repository::discover` +
/// `RepoStatus::compute` otherwise.
#[must_use]
pub fn render(discover_from: &std::path::Path) -> String {
    let status = crate::daemon::status_for_cwd(discover_from);
    let (out, _) = render_segment(status.as_ref(), Some(237));
    format!("{out}{RST}")
}

#[cfg(test)]
mod tests {
    use super::{OpState, RepoStatus, render, render_segment};
    use crate::color::{ARROW, BRANCH_ICON, RST, bg, fg};
    use crate::segments::testutil::init_repo;
    use std::fs;
    use tempfile::TempDir;

    // ── Helpers ─────────────────────────────────────────────────────────

    fn clean_status() -> RepoStatus {
        RepoStatus {
            repo_name: "chevron".to_string(),
            branch: "master".to_string(),
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

    // ── Behavior tests (using top-level render() against real repos) ────

    #[test]
    fn not_a_repo() {
        let tmp = TempDir::new().unwrap();
        let out = render(tmp.path());
        assert!(out.contains(ARROW));
        assert!(out.contains(RST));
        assert!(!out.contains(BRANCH_ICON));
    }

    #[test]
    fn clean_repo_green() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let out = render(tmp.path());
        assert!(out.contains(bg(148)), "expected green bg(148) in: {out}");
        assert!(out.contains(BRANCH_ICON));
    }

    #[test]
    fn modified_file_shows_pencil_count() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        let file_path = tmp.path().join("file.txt");
        fs::write(&file_path, "hello").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&head])
            .unwrap();

        fs::write(&file_path, "modified").unwrap();

        let out = render(tmp.path());
        assert!(out.contains(bg(161)), "expected pink bg(161) in: {out}");
        assert!(out.contains('\u{270E}'), "expected pencil icon in: {out}");
    }

    #[test]
    fn filtered_file_not_counted_as_modified() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        fs::write(
            tmp.path().join(".gitattributes"),
            "secret.md filter=crypt diff=crypt\n",
        )
        .unwrap();
        fs::write(tmp.path().join("secret.md"), "plaintext").unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new(".gitattributes"))
            .unwrap();
        index.add_path(std::path::Path::new("secret.md")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "add filtered file",
            &tree,
            &[&head],
        )
        .unwrap();

        fs::write(tmp.path().join("secret.md"), "decrypted plaintext").unwrap();

        let out = render(tmp.path());
        assert!(
            !out.contains('\u{270E}'),
            "filtered file should not show as modified: {out}"
        );
        assert!(
            out.contains(bg(148)),
            "repo should appear clean (green): {out}"
        );
    }

    #[test]
    fn staged_file_shows_checkmark() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        let file_path = tmp.path().join("new.txt");
        fs::write(&file_path, "new").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("new.txt")).unwrap();
        index.write().unwrap();

        let out = render(tmp.path());
        assert!(out.contains('\u{2714}'), "expected checkmark in: {out}");
    }

    #[test]
    fn untracked_file_shows_plus() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        fs::write(tmp.path().join("untracked.txt"), "x").unwrap();

        let out = render(tmp.path());
        assert!(out.contains('+'), "expected + for untracked in: {out}");
    }

    #[test]
    fn clean_repo_with_stash_is_green_with_stash_indicator() {
        let tmp = TempDir::new().unwrap();
        let mut repo = init_repo(tmp.path());

        let file_path = tmp.path().join("file.txt");
        fs::write(&file_path, "hello").unwrap();
        let sig = {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_id).unwrap();
            let head = repo.head().unwrap().peel_to_commit().unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "add file", &tree, &[&head])
                .unwrap();
            sig
        };

        fs::write(&file_path, "modified").unwrap();
        repo.stash_save(&sig, "wip", None).unwrap();

        let out = render(tmp.path());
        assert!(out.contains(bg(148)), "expected green bg(148) in: {out}");
        assert!(out.contains('\u{2691}'), "expected stash icon in: {out}");
        assert!(!out.contains(bg(161)), "should not be pink: {out}");
    }

    #[test]
    fn detached_head_shows_short_sha() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        let head_oid = repo.head().unwrap().target().unwrap();
        let short_sha = head_oid.to_string()[..7].to_string();
        repo.set_head_detached(head_oid).unwrap();

        let out = render(tmp.path());
        assert!(
            out.contains(&short_sha),
            "expected short SHA {short_sha} in: {out}"
        );
        assert!(
            out.contains(BRANCH_ICON),
            "expected branch icon in detached state: {out}"
        );
    }

    // ── RepoStatus::compute tests ───────────────────────────────────────

    #[test]
    fn compute_clean_repo() {
        let tmp = TempDir::new().unwrap();
        let mut repo = init_repo(tmp.path());

        let s = RepoStatus::compute(&mut repo);
        assert!(!s.repo_name.is_empty());
        assert!(!s.detached);
        assert_eq!(s.state, None);
        assert_eq!(
            (
                s.staged,
                s.modified,
                s.untracked,
                s.conflicted,
                s.ahead,
                s.behind,
                s.stashed
            ),
            (0, 0, 0, 0, 0, 0, 0)
        );
        assert!(!s.is_dirty());
    }

    #[test]
    fn compute_detached_sets_flag_and_sha_branch() {
        let tmp = TempDir::new().unwrap();
        let mut repo = init_repo(tmp.path());

        let head_oid = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(head_oid).unwrap();

        let s = RepoStatus::compute(&mut repo);
        assert!(s.detached, "detached flag should be set");
        assert_eq!(
            s.branch.len(),
            7,
            "branch should be 7-char SHA: {}",
            s.branch
        );
        assert!(s.branch.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_counts_untracked() {
        let tmp = TempDir::new().unwrap();
        let mut repo = init_repo(tmp.path());
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let s = RepoStatus::compute(&mut repo);
        assert_eq!(s.untracked, 2);
        assert_eq!(s.staged, 0);
        assert_eq!(s.modified, 0);
        assert!(s.is_dirty());
    }

    #[test]
    fn is_dirty_excludes_stashes() {
        let s = RepoStatus {
            stashed: 3,
            ..clean_status()
        };
        assert!(!s.is_dirty(), "stashes alone should not flip dirty");
    }

    #[test]
    fn is_dirty_set_by_op_state() {
        let s = RepoStatus {
            state: Some(OpState::Rebasing),
            ..clean_status()
        };
        assert!(s.is_dirty(), "in-flight rebase should flip dirty");
    }

    // ── render_segment tests (pure, no libgit2) ─────────────────────────

    #[test]
    fn render_segment_no_status_emits_closing_arrow() {
        let (out, end_bg) = render_segment(None, Some(240));
        assert!(out.contains(fg(240)), "expected fg(240) in: {out}");
        assert!(out.contains(ARROW));
        assert_eq!(end_bg, Some(236));
    }

    // ── Snapshot tests over hand-crafted RepoStatus values ──────────────
    // These lock down the exact ANSI byte sequence rendered for canonical
    // scenarios. Regenerate with `cargo insta accept` after intentional
    // format changes.

    #[test]
    fn snap_segment_clean_no_stash() {
        let (out, _) = render_segment(Some(&clean_status()), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_clean_with_stash() {
        let s = RepoStatus {
            stashed: 2,
            ..clean_status()
        };
        let (out, _) = render_segment(Some(&s), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_dirty_just_modified() {
        let s = RepoStatus {
            modified: 3,
            ..clean_status()
        };
        let (out, _) = render_segment(Some(&s), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_dirty_all_chips() {
        let s = RepoStatus {
            branch: "feature/foo".to_string(),
            staged: 1,
            modified: 2,
            untracked: 3,
            conflicted: 1,
            ahead: 4,
            behind: 5,
            stashed: 6,
            ..clean_status()
        };
        let (out, _) = render_segment(Some(&s), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_rebasing() {
        let s = RepoStatus {
            state: Some(OpState::Rebasing),
            modified: 1,
            ..clean_status()
        };
        let (out, _) = render_segment(Some(&s), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_detached_head() {
        let s = RepoStatus {
            branch: "abc1234".to_string(),
            detached: true,
            ..clean_status()
        };
        let (out, _) = render_segment(Some(&s), Some(237));
        insta::assert_snapshot!(out);
    }

    #[test]
    fn snap_segment_no_repo() {
        let (out, _) = render_segment(None, Some(237));
        insta::assert_snapshot!(out);
    }
}
