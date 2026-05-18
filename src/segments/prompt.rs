use git2::Repository;

use crate::config::Config;
use crate::segments::{git, registry, reset, tmux_title};

pub struct PromptContext {
    pub home: String,
    pub pwd: String,
    pub max_dir_size: Option<usize>,
    pub repo: Option<Repository>,
    pub exit_status: i32,
    pub duration_ms: u64,
    pub job_count: u32,
    pub in_tmux: bool,
    /// Set by the git segment during rendering, read by tmux title.
    pub repo_status: Option<git::RepoStatus>,
    pub config: Config,
}

impl PromptContext {
    #[must_use]
    pub fn gather(
        max_dir_size: Option<usize>,
        exit_status: i32,
        duration_ms: u64,
        job_count: u32,
    ) -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let pwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let repo = Repository::discover(".").ok();
        let in_tmux = std::env::var("TMUX").is_ok();
        Self {
            home,
            pwd,
            max_dir_size,
            repo,
            exit_status,
            duration_ms,
            job_count,
            in_tmux,
            repo_status: None,
            config: Config::load(),
        }
    }
}

#[must_use]
pub fn render(ctx: &mut PromptContext) -> String {
    let segments = registry::build_segments(&ctx.config);
    let mut out = String::with_capacity(1024);
    let mut from_bg: Option<u8> = None;

    for seg in &segments {
        // Catch panics per-segment so a bug in one segment doesn't drop the
        // entire prompt. The panicked segment renders as empty (pass-through
        // `from_bg`) and the rest of the chain continues. PromptContext
        // contains a libgit2 Repository that isn't UnwindSafe by default;
        // AssertUnwindSafe is acceptable here because we don't observe ctx
        // state after a panic (the next segment gets the same ctx, and any
        // partial mutation a panicking segment performed is bounded to fields
        // a well-behaved segment would touch).
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| seg.render(ctx, from_bg)))
                .unwrap_or_else(|_| registry::SegmentOutput {
                    text: String::new(),
                    end_bg: from_bg,
                });
        out.push_str(&result.text);
        from_bg = result.end_bg;
    }

    out.push_str(&reset::render_final(from_bg));

    if ctx.in_tmux {
        let title = tmux_title::render_from_status(&ctx.home, &ctx.pwd, ctx.repo_status.as_ref());
        out.push('\n');
        out.push_str(&title);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{PromptContext, render};
    use crate::color::{ARROW, BRANCH_ICON, RST};
    use crate::config::Config;
    use crate::segments::testutil::init_repo;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn renders_path_and_git() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };

        let mut ctx = PromptContext {
            home: "/home/user".to_string(),
            pwd: tmp.path().to_string_lossy().to_string(),
            max_dir_size: None,
            repo: Some(repo),
            exit_status: 0,
            duration_ms: 0,
            job_count: 0,
            in_tmux: false,
            repo_status: None,
            config: Config::default(),
        };

        let out = render(&mut ctx);
        assert!(out.contains(ARROW), "expected arrows in: {out}");
        assert!(out.contains(BRANCH_ICON), "expected branch icon in: {out}");
    }

    #[test]
    #[serial]
    fn renders_without_repo() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };

        let mut ctx = PromptContext {
            home: "/home/user".to_string(),
            pwd: "/tmp".to_string(),
            max_dir_size: None,
            repo: None,
            exit_status: 0,
            duration_ms: 0,
            job_count: 0,
            in_tmux: false,
            repo_status: None,
            config: Config::default(),
        };

        let out = render(&mut ctx);
        assert!(out.contains(ARROW), "expected arrows in: {out}");
        assert!(!out.contains(BRANCH_ICON), "should not contain branch icon");
    }

    #[test]
    #[serial]
    fn tmux_mode_appends_title_line() {
        let tmp = TempDir::new().unwrap();
        let repo = init_repo(tmp.path());

        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };

        let mut ctx = PromptContext {
            home: "/home/user".to_string(),
            pwd: tmp.path().to_string_lossy().to_string(),
            max_dir_size: None,
            repo: Some(repo),
            exit_status: 0,
            duration_ms: 0,
            job_count: 0,
            in_tmux: true,
            repo_status: None,
            config: Config::default(),
        };

        let out = render(&mut ctx);
        let lines: Vec<&str> = out.splitn(2, '\n').collect();
        assert_eq!(lines.len(), 2, "expected two lines in tmux mode: {out}");
        assert!(
            lines[0].contains(BRANCH_ICON),
            "prompt line should have branch"
        );
        assert!(
            lines[1].contains(BRANCH_ICON),
            "tmux title should have branch: {}",
            lines[1]
        );
    }

    #[test]
    #[serial]
    fn no_tmux_single_line() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };

        let mut ctx = PromptContext {
            home: "/home/user".to_string(),
            pwd: "/tmp".to_string(),
            max_dir_size: None,
            repo: None,
            exit_status: 0,
            duration_ms: 0,
            job_count: 0,
            in_tmux: false,
            repo_status: None,
            config: Config::default(),
        };

        let out = render(&mut ctx);
        assert!(!out.contains('\n'), "non-tmux output should be single line");
    }

    #[test]
    #[serial]
    fn full_chain_ends_with_reset() {
        // SAFETY: test-only
        unsafe { std::env::remove_var("IN_NIX_SHELL") };

        let mut ctx = PromptContext {
            home: "/home/user".to_string(),
            pwd: "/home/user/projects".to_string(),
            max_dir_size: Some(20),
            repo: None,
            exit_status: 0,
            duration_ms: 0,
            job_count: 0,
            in_tmux: false,
            repo_status: None,
            config: Config::default(),
        };

        let out = render(&mut ctx);
        assert!(out.ends_with(RST), "should end with reset: {out}");
    }
}
