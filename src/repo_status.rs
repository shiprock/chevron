use std::fmt::Write;
use std::process::Command;

use git2::Repository;

pub fn run() {
    let Ok(repo) = Repository::discover(".") else {
        eprintln!("not a git repository");
        std::process::exit(1);
    };

    let mut out = String::with_capacity(2048);

    render_header(&repo, &mut out);
    render_branch_status(&repo, &mut out);
    render_recent_commits(&repo, &mut out);
    render_drift(&repo, &mut out);

    if has_gh() {
        render_open_prs(&mut out);
        render_ci_status(&mut out);
    }

    print!("{out}");
}

fn render_header(repo: &Repository, out: &mut String) {
    let name = repo
        .workdir()
        .and_then(|p| p.file_name())
        .map_or_else(String::new, |n| n.to_string_lossy().to_string());

    let branch = current_branch(repo);

    let _ = writeln!(out, "\x1b[1m{name}\x1b[0m on \x1b[36m{branch}\x1b[0m");
    let _ = writeln!(out);
}

fn render_branch_status(repo: &Repository, out: &mut String) {
    let (ahead, behind) = ahead_behind(repo);

    if ahead == 0 && behind == 0 {
        let _ = writeln!(out, "  \x1b[32mup to date with remote\x1b[0m");
    } else {
        if ahead > 0 {
            let _ = writeln!(
                out,
                "  \x1b[33m{ahead} commit{} ahead\x1b[0m",
                plural(u64::from(ahead))
            );
        }
        if behind > 0 {
            let _ = writeln!(
                out,
                "  \x1b[33m{behind} commit{} behind\x1b[0m",
                plural(u64::from(behind))
            );
        }
    }
    let _ = writeln!(out);
}

fn render_recent_commits(repo: &Repository, out: &mut String) {
    let _ = writeln!(out, "\x1b[1mRecent commits:\x1b[0m");

    let Ok(mut revwalk) = repo.revwalk() else {
        return;
    };
    let _ = revwalk.push_head();

    let mut count = 0;
    for oid in revwalk.flatten().take(5) {
        let Ok(commit) = repo.find_commit(oid) else {
            continue;
        };
        let short_id = &commit.id().to_string()[..7];
        let summary = commit.summary().ok().flatten().unwrap_or("");
        let time = commit.time();
        let age = format_age(time.seconds());

        let _ = writeln!(
            out,
            "  \x1b[33m{short_id}\x1b[0m {summary} \x1b[90m({age})\x1b[0m"
        );
        count += 1;
    }

    if count == 0 {
        let _ = writeln!(out, "  (no commits)");
    }
    let _ = writeln!(out);
}

fn render_drift(repo: &Repository, out: &mut String) {
    // Find how many commits on this branch are not on main/master
    let branch = current_branch(repo);
    let main = find_main_branch(repo);

    if branch == main {
        return;
    }

    let Ok(branch_oid) = repo.revparse_single(&branch).map(|o| o.id()) else {
        return;
    };
    let Ok(main_oid) = repo.revparse_single(&main).map(|o| o.id()) else {
        return;
    };

    if let Ok((ahead, behind)) = repo.graph_ahead_behind(branch_oid, main_oid) {
        let _ = writeln!(out, "\x1b[1mDrift from {main}:\x1b[0m");
        if ahead == 0 && behind == 0 {
            let _ = writeln!(out, "  \x1b[32meven\x1b[0m");
        } else {
            if ahead > 0 {
                let _ = writeln!(
                    out,
                    "  \x1b[36m{ahead} commit{} ahead\x1b[0m",
                    plural(ahead as u64)
                );
            }
            if behind > 0 {
                let _ = writeln!(
                    out,
                    "  \x1b[33m{behind} commit{} behind\x1b[0m",
                    plural(behind as u64)
                );
            }
        }
        let _ = writeln!(out);
    }
}

fn render_open_prs(out: &mut String) {
    // Use --jq to format each PR as: number\ttitle\tbranch\tci_state
    // ci_state is one of: passing, running, failing, unknown
    let jq = concat!(
        ".[] | [(.number | tostring), .title, .headRefName, ",
        "(if (.statusCheckRollup // [] | length) == 0 then \"unknown\" ",
        "elif (.statusCheckRollup | all(.conclusion == \"SUCCESS\")) then \"passing\" ",
        "elif (.statusCheckRollup | any(.status == \"IN_PROGRESS\")) then \"running\" ",
        "else \"failing\" end)] | @tsv",
    );

    let result = Command::new("gh")
        .args([
            "pr",
            "list",
            "--author",
            "@me",
            "--state",
            "open",
            "--limit",
            "10",
            "--json",
            "number,title,headRefName,statusCheckRollup",
            "--jq",
            jq,
        ])
        .output();

    let Ok(output) = result else { return };
    if !output.status.success() {
        return;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return;
    }

    let _ = writeln!(out, "\x1b[1mOpen PRs:\x1b[0m");
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let (number, title, branch, ci_state) = (parts[0], parts[1], parts[2], parts[3]);

        let ci = match ci_state {
            "passing" => " [\x1b[32mpassing\x1b[0m]",
            "running" => " [\x1b[33mrunning\x1b[0m]",
            "failing" => " [\x1b[31mfailing\x1b[0m]",
            _ => "",
        };

        let _ = writeln!(out, "  \x1b[36m#{number}\x1b[0m {title} ({branch}){ci}");
    }
    let _ = writeln!(out);
}

fn render_ci_status(out: &mut String) {
    // Use --jq to format each check as: name\tstate\tconclusion
    let result = Command::new("gh")
        .args([
            "pr",
            "checks",
            "--json",
            "name,state,conclusion",
            "--jq",
            ".[] | [.name, .state, .conclusion] | @tsv",
        ])
        .output();

    let Ok(output) = result else { return };
    if !output.status.success() {
        return;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return;
    }

    let _ = writeln!(out, "\x1b[1mCI checks:\x1b[0m");
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let (name, state, conclusion) = (parts[0], parts[1], parts[2]);

        let icon = match conclusion {
            "SUCCESS" => "\x1b[32m\u{2713}\x1b[0m",
            "FAILURE" => "\x1b[31m\u{2717}\x1b[0m",
            _ if state == "IN_PROGRESS" => "\x1b[33m\u{25cf}\x1b[0m",
            _ => "\x1b[90m\u{25cb}\x1b[0m",
        };

        let _ = writeln!(out, "  {icon} {name}");
    }
    let _ = writeln!(out);
}

fn has_gh() -> bool {
    Command::new("gh")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn current_branch(repo: &Repository) -> String {
    if repo.head_detached().unwrap_or(false) {
        return repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok())
            .map_or_else(
                || "HEAD".to_string(),
                |c| c.id().to_string()[..7].to_string(),
            );
    }
    repo.head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(str::to_string))
        .unwrap_or_else(|| "HEAD".to_string())
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

fn find_main_branch(repo: &Repository) -> String {
    for name in ["main", "master"] {
        if repo.find_branch(name, git2::BranchType::Local).is_ok() {
            return name.to_string();
        }
    }
    "main".to_string()
}

fn format_age(epoch_secs: i64) -> String {
    let now: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let epoch_u64 = u64::try_from(epoch_secs).unwrap_or(0);
    let delta = now.saturating_sub(epoch_u64);

    if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        let m = delta / 60;
        format!("{m} min{} ago", plural(m))
    } else if delta < 86400 {
        let h = delta / 3600;
        format!("{h} hour{} ago", plural(h))
    } else {
        let d = delta / 86400;
        format!("{d} day{} ago", plural(d))
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::{
        current_branch, find_main_branch, format_age, plural, render_branch_status, render_drift,
        render_header, render_recent_commits,
    };
    use crate::segments::testutil::init_repo;
    use tempfile::TempDir;

    // --- plural ---

    #[test]
    fn plural_one_is_empty() {
        assert_eq!(plural(1), "");
    }

    #[test]
    fn plural_zero_and_many_is_s() {
        assert_eq!(plural(0), "s");
        assert_eq!(plural(2), "s");
        assert_eq!(plural(100), "s");
    }

    // --- format_age ---

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs().cast_signed())
    }

    #[test]
    fn format_age_just_now() {
        let out = format_age(now_secs());
        assert_eq!(out, "just now");
    }

    #[test]
    fn format_age_minutes() {
        let out = format_age(now_secs() - 300); // 5 min ago
        assert!(out.contains("min"), "expected 'min' in: {out}");
        assert!(out.contains("ago"), "expected 'ago' in: {out}");
        assert!(out.contains('5'), "expected '5' in: {out}");
    }

    #[test]
    fn format_age_one_minute_singular() {
        let out = format_age(now_secs() - 60);
        assert!(out.contains("1 min ago"), "expected '1 min ago' in: {out}");
    }

    #[test]
    fn format_age_hours() {
        let out = format_age(now_secs() - 7200); // 2 hours ago
        assert!(out.contains("hour"), "expected 'hour' in: {out}");
        assert!(out.contains('2'), "expected '2' in: {out}");
    }

    #[test]
    fn format_age_one_hour_singular() {
        let out = format_age(now_secs() - 3600);
        assert!(
            out.contains("1 hour ago"),
            "expected '1 hour ago' in: {out}"
        );
    }

    #[test]
    fn format_age_days() {
        let out = format_age(now_secs() - 3 * 86400); // 3 days ago
        assert!(out.contains("day"), "expected 'day' in: {out}");
        assert!(out.contains('3'), "expected '3' in: {out}");
    }

    #[test]
    fn format_age_one_day_singular() {
        let out = format_age(now_secs() - 86400);
        assert!(out.contains("1 day ago"), "expected '1 day ago' in: {out}");
    }

    // --- find_main_branch ---

    #[test]
    fn find_main_branch_finds_initial_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let branch = find_main_branch(&repo);
        assert!(
            branch == "master" || branch == "main",
            "expected main or master, got: {branch}"
        );
    }

    #[test]
    fn find_main_branch_falls_back_to_main() {
        // A repo with a branch named "feature" has neither main nor master
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        // Rename the current branch away from main/master
        let head_ref = repo.head().unwrap().target().unwrap();
        repo.branch("feature", &repo.find_commit(head_ref).unwrap(), false)
            .unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        // Delete original branch
        let orig = find_main_branch(&repo);
        let mut orig_branch = repo.find_branch(&orig, git2::BranchType::Local).unwrap();
        orig_branch.delete().unwrap();

        let branch = find_main_branch(&repo);
        assert_eq!(
            branch, "main",
            "should fall back to 'main' when neither exists"
        );
    }

    // --- current_branch ---

    #[test]
    fn current_branch_returns_branch_name() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let branch = current_branch(&repo);
        assert!(
            branch == "master" || branch == "main",
            "expected branch name, got: {branch}"
        );
    }

    #[test]
    fn current_branch_detached_returns_short_sha() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let head_oid = repo.head().unwrap().target().unwrap();
        let expected = head_oid.to_string()[..7].to_string();
        repo.set_head_detached(head_oid).unwrap();
        let branch = current_branch(&repo);
        assert_eq!(branch, expected, "detached HEAD should show short SHA");
    }

    // --- render_header ---

    #[test]
    fn render_header_contains_branch_and_repo_name() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let mut out = String::new();
        render_header(&repo, &mut out);
        assert!(!out.is_empty());
        assert!(
            out.contains("master") || out.contains("main"),
            "expected branch name in: {out}"
        );
    }

    // --- render_branch_status ---

    #[test]
    fn render_branch_status_no_upstream_is_up_to_date() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let mut out = String::new();
        render_branch_status(&repo, &mut out);
        assert!(
            out.contains("up to date"),
            "expected 'up to date' in: {out}"
        );
    }

    // --- render_recent_commits ---

    #[test]
    fn render_recent_commits_shows_initial_commit() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let mut out = String::new();
        render_recent_commits(&repo, &mut out);
        assert!(out.contains("Recent commits"), "expected header in: {out}");
        assert!(
            out.contains("init"),
            "expected initial commit message in: {out}"
        );
    }

    // --- render_drift ---

    #[test]
    fn render_drift_on_main_branch_is_silent() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());
        let mut out = String::new();
        render_drift(&repo, &mut out);
        assert!(
            out.is_empty(),
            "render_drift should be silent when on main branch: {out}"
        );
    }

    #[test]
    fn render_drift_on_feature_branch_shows_ahead() {
        use std::fs;
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        // Create and switch to a feature branch
        let head_oid = repo.head().unwrap().target().unwrap();
        let head_commit = repo.find_commit(head_oid).unwrap();
        repo.branch("feature", &head_commit, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();

        // Add a commit on the feature branch
        fs::write(tmp.path().join("feat.txt"), "x").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("feat.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "feat: add file",
            &tree,
            &[&head_commit],
        )
        .unwrap();

        let mut out = String::new();
        render_drift(&repo, &mut out);
        assert!(out.contains("Drift"), "expected 'Drift' in: {out}");
        assert!(out.contains("ahead"), "expected 'ahead' in: {out}");
    }
}
