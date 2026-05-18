//! In-process benches for the hot prompt-render paths.
//!
//! Run with `cargo bench` (or `cargo bench --bench segments path` to filter).
//! These measure individual functions inside the binary — for end-to-end
//! process-spawn timing, use `scripts/bench.sh` (hyperfine).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc
)]

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use git2::Repository;
use std::fs;
use tempfile::TempDir;

use plx::config::Config;
use plx::segments::{git, path, prompt, tmux_title};

/// Minimal git repo with a single empty commit, mirroring `segments::testutil::init_repo`
/// (which lives behind `#[cfg(test)]` and isn't reachable from a separate bench crate).
fn init_repo(dir: &std::path::Path) -> Repository {
    let repo = Repository::init(dir).expect("repo init");
    {
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "bench").unwrap();
        cfg.set_str("user.email", "bench@example.com").unwrap();
    }
    let sig = repo.signature().unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    drop(tree);
    repo
}

fn bench_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("path");
    group.bench_function("home_root", |b| {
        b.iter(|| path::render(black_box("/home/user"), black_box("/home/user"), None));
    });
    group.bench_function("home_one_deep", |b| {
        b.iter(|| {
            path::render(
                black_box("/home/user"),
                black_box("/home/user/src/plx"),
                None,
            )
        });
    });
    group.bench_function("non_home_deep", |b| {
        b.iter(|| {
            path::render(
                black_box(""),
                black_box("/var/log/nginx/error.log"),
                None,
            )
        });
    });
    group.bench_function("very_deep_truncated", |b| {
        b.iter(|| {
            path::render(
                black_box(""),
                black_box("/a/b/c/d/e/f/g/h/i/j/k"),
                None,
            )
        });
    });
    group.bench_function("with_max_dir_size", |b| {
        b.iter(|| {
            path::render(
                black_box("/home/user"),
                black_box("/home/user/very-long-dir-name/sub"),
                Some(8),
            )
        });
    });
    group.finish();
}

fn bench_git(c: &mut Criterion) {
    let mut group = c.benchmark_group("git");

    // Clean repo — measures the baseline cost of libgit2 status when there's
    // nothing to report. Most-common case during interactive editing.
    let clean = TempDir::new().unwrap();
    init_repo(clean.path());
    let clean_path = clean.path().to_path_buf();
    group.bench_function("render_clean", |b| {
        b.iter(|| git::render(black_box(&clean_path)));
    });

    let clean_repo = Repository::open(clean.path()).unwrap();
    group.bench_function("gather_clean", |b| {
        b.iter(|| git::GitInfo::gather(black_box(&clean_repo)));
    });

    // Dirty repo with 20 untracked files — exercises the per-entry loop.
    let dirty = TempDir::new().unwrap();
    init_repo(dirty.path());
    for i in 0..20 {
        fs::write(dirty.path().join(format!("f{i}.txt")), "x").unwrap();
    }
    let dirty_path = dirty.path().to_path_buf();
    group.bench_function("render_20_untracked", |b| {
        b.iter(|| git::render(black_box(&dirty_path)));
    });

    let dirty_repo = Repository::open(dirty.path()).unwrap();
    group.bench_function("gather_20_untracked", |b| {
        b.iter(|| git::GitInfo::gather(black_box(&dirty_repo)));
    });

    // Real-world: bench against the plx working tree itself if we happen to be
    // in a checkout of it. Gives a representative number for a moderately-sized
    // repo with realistic .gitignore patterns, sub-trees, etc.
    if let Ok(repo) = Repository::discover(".") {
        group.bench_function("gather_cwd", |b| {
            b.iter(|| git::GitInfo::gather(black_box(&repo)));
        });
    }

    group.finish();
}

fn bench_tmux_title(c: &mut Criterion) {
    let mut group = c.benchmark_group("tmux_title");

    // Pure render — no repo discovery, no libgit2 calls.
    let info = git::GitInfo {
        repo_name: "plx".to_string(),
        branch: "master".to_string(),
        dirty: false,
    };
    group.bench_function("from_info_clean", |b| {
        b.iter(|| {
            tmux_title::render_from_info(
                black_box("/home/user"),
                black_box("/home/user/src/plx"),
                Some(black_box(&info)),
            )
        });
    });

    // Full render() including Repository::discover + GitInfo::gather.
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let pwd = tmp.path().to_string_lossy().to_string();
    group.bench_function("render_full_clean", |b| {
        b.iter(|| tmux_title::render(black_box("/nonexistent"), black_box(&pwd)));
    });

    group.finish();
}

fn bench_prompt(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    let pwd = tmp.path().to_string_lossy().to_string();

    let mut group = c.benchmark_group("prompt");

    // The whole prompt rendered against a fresh clean repo. iter_batched so we
    // re-open the Repository each iteration (PromptContext takes ownership and
    // render() mutates git_info).
    group.bench_function("render_clean_repo", |b| {
        b.iter_batched(
            || prompt::PromptContext {
                home: "/home/user".to_string(),
                pwd: pwd.clone(),
                max_dir_size: None,
                repo: Repository::open(tmp.path()).ok(),
                exit_status: 0,
                duration_ms: 0,
                job_count: 0,
                in_tmux: false,
                git_info: None,
                config: Config::default(),
            },
            |mut ctx| prompt::render(black_box(&mut ctx)),
            BatchSize::SmallInput,
        );
    });

    // No-repo case (e.g. /tmp): exercises the segment chain when git is absent.
    let no_repo = TempDir::new().unwrap();
    let no_repo_pwd = no_repo.path().to_string_lossy().to_string();
    group.bench_function("render_no_repo", |b| {
        b.iter_batched(
            || prompt::PromptContext {
                home: "/home/user".to_string(),
                pwd: no_repo_pwd.clone(),
                max_dir_size: None,
                repo: None,
                exit_status: 0,
                duration_ms: 0,
                job_count: 0,
                in_tmux: false,
                git_info: None,
                config: Config::default(),
            },
            |mut ctx| prompt::render(black_box(&mut ctx)),
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_path,
    bench_git,
    bench_tmux_title,
    bench_prompt
);
criterion_main!(benches);
