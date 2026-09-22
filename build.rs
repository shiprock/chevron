//! Embeds a build identifier so `chevron version`, `chevron doctor` and the
//! daemon's `VERSION` reply can tell two builds of the same crate version
//! apart. Resolution order: `CHEVRON_BUILD_ID` from the environment (the Nix
//! flake passes its own revision, since the sandbox has no `.git`), then
//! `git rev-parse` on the checkout with a `-dirty` suffix when tracked files
//! have uncommitted changes, then `unknown`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CHEVRON_BUILD_ID");
    let id = std::env::var("CHEVRON_BUILD_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(from_git)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CHEVRON_BUILD_ID={id}");
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn from_git() -> Option<String> {
    // Rebuild when HEAD moves: the symbolic HEAD file, the ref it names,
    // and packed-refs in case that ref is packed.
    for path in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["rev-parse", "--git-path", "packed-refs"]),
        git(&["symbolic-ref", "-q", "HEAD"])
            .and_then(|branch| git(&["rev-parse", "--git-path", &branch])),
    ]
    .into_iter()
    .flatten()
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let sha = git(&["rev-parse", "--short=12", "HEAD"]).filter(|s| !s.is_empty())?;
    let dirty =
        git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}
