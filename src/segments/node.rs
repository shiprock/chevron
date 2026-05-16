use std::fmt::Write;

use crate::color::{arrow, fg};
use crate::segments::find_ancestor_file;
use crate::segments::prompt::PromptContext;
use crate::segments::registry::{Segment, SegmentOutput};

const MY_BG: u8 = 70;
const MY_FG: u8 = 15;
const ICON: &str = "\u{2B22}"; // Black hexagon

pub struct NodeSegment;

impl Segment for NodeSegment {
    fn name(&self) -> &'static str {
        "node"
    }

    fn render(&self, ctx: &mut PromptContext, from_bg: Option<u8>) -> SegmentOutput {
        let empty = SegmentOutput {
            text: String::new(),
            end_bg: from_bg,
        };

        // Only show in Node projects
        if find_ancestor_file(&ctx.pwd, "package.json", 5).is_none() {
            return empty;
        }

        let Some(version) = read_node_version(&ctx.pwd) else {
            return empty;
        };

        let mut out = String::with_capacity(128);
        let _ = write!(
            out,
            "{} {}{ICON} {version} ",
            arrow(from_bg, MY_BG),
            fg(MY_FG),
        );
        SegmentOutput {
            text: out,
            end_bg: Some(MY_BG),
        }
    }
}

fn read_node_version(pwd: &str) -> Option<String> {
    // .node-version file (fnm, nodenv)
    if let Some(path) = find_ancestor_file(pwd, ".node-version", 5)
        && let Some(v) = read_trimmed(&path)
    {
        return Some(v);
    }

    // .nvmrc file (nvm)
    if let Some(path) = find_ancestor_file(pwd, ".nvmrc", 5)
        && let Some(v) = read_trimmed(&path)
    {
        return Some(v);
    }

    // NODE_VERSION env var (set by nvm/fnm on activation)
    std::env::var("NODE_VERSION").ok().filter(|v| !v.is_empty())
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = contents.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn no_package_json_returns_none() {
        let tmp = TempDir::new().unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        // Serial: races with reads_node_version_env, which sets NODE_VERSION.
        unsafe { std::env::remove_var("NODE_VERSION") };
        assert!(read_node_version(&pwd).is_none());
    }

    #[test]
    fn reads_node_version_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".node-version"), "20.11.0\n").unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_node_version(&pwd).unwrap(), "20.11.0");
    }

    #[test]
    fn reads_nvmrc_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".nvmrc"), "lts/iron\n").unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_node_version(&pwd).unwrap(), "lts/iron");
    }

    #[test]
    fn node_version_file_takes_precedence_over_nvmrc() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".node-version"), "20.11.0\n").unwrap();
        std::fs::write(tmp.path().join(".nvmrc"), "18.0.0\n").unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_node_version(&pwd).unwrap(), "20.11.0");
    }

    #[test]
    #[serial]
    fn reads_node_version_env() {
        let tmp = TempDir::new().unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        unsafe { std::env::set_var("NODE_VERSION", "21.0.0") };
        assert_eq!(read_node_version(&pwd).unwrap(), "21.0.0");
        unsafe { std::env::remove_var("NODE_VERSION") };
    }

    #[test]
    fn empty_version_file_skipped() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".node-version"), "  \n").unwrap();
        std::fs::write(tmp.path().join(".nvmrc"), "18.0.0\n").unwrap();
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_node_version(&pwd).unwrap(), "18.0.0");
    }
}
