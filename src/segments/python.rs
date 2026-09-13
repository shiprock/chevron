use std::fmt::Write;
use std::path::PathBuf;

use crate::color::{arrow, fg};
use crate::segments::find_ancestor_file;
use crate::segments::prompt::PromptContext;
use crate::segments::registry::{Segment, SegmentOutput};

const MY_BG: u8 = 24;
const MY_FG: u8 = 220;
const ICON: &str = "\u{E73C}"; // Nerd Font Python icon

pub struct PythonSegment;

impl Segment for PythonSegment {
    fn name(&self) -> &'static str {
        "python"
    }

    fn render(&self, ctx: &mut PromptContext, from_bg: Option<u8>) -> SegmentOutput {
        let empty = SegmentOutput {
            text: String::new(),
            end_bg: from_bg,
        };

        // Only show in Python projects or active venvs
        let in_venv = std::env::var("VIRTUAL_ENV").is_ok_and(|v| !v.is_empty());

        let in_project = find_ancestor_file(&ctx.pwd, "pyproject.toml", 5).is_some()
            || find_ancestor_file(&ctx.pwd, "setup.py", 5).is_some()
            || find_ancestor_file(&ctx.pwd, "requirements.txt", 5).is_some();

        if !in_venv && !in_project {
            return empty;
        }

        let Some(version) = read_python_version(&ctx.pwd) else {
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

fn read_python_version(pwd: &str) -> Option<String> {
    // pyvenv.cfg in the active venv (has "version = 3.x.y")
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let cfg_path = PathBuf::from(&venv).join("pyvenv.cfg");
        if let Ok(contents) = std::fs::read_to_string(cfg_path) {
            for line in contents.lines() {
                if let Some(val) = line.strip_prefix("version") {
                    let val = val.trim_start_matches([' ', '=']);
                    let val = val.trim();
                    if !val.is_empty() {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }

    // .python-version file (pyenv)
    if let Some(path) = find_ancestor_file(pwd, ".python-version", 5)
        && let Ok(contents) = std::fs::read_to_string(path)
    {
        let trimmed = contents.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn reads_pyvenv_cfg_version() {
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(
            venv.join("pyvenv.cfg"),
            "home = /usr/bin\nversion = 3.12.1\ninclude-system-site-packages = false\n",
        )
        .unwrap();

        unsafe { std::env::set_var("VIRTUAL_ENV", venv.to_str().unwrap()) };
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_python_version(&pwd).unwrap(), "3.12.1");
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
    }

    #[test]
    #[serial]
    fn reads_python_version_file() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".python-version"), "3.11.0\n").unwrap();

        unsafe { std::env::remove_var("VIRTUAL_ENV") };
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_python_version(&pwd).unwrap(), "3.11.0");
    }

    #[test]
    #[serial]
    fn pyvenv_cfg_takes_precedence() {
        let tmp = TempDir::new().unwrap();
        let venv = tmp.path().join("venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(venv.join("pyvenv.cfg"), "version = 3.12.0\n").unwrap();
        std::fs::write(tmp.path().join(".python-version"), "3.11.0\n").unwrap();

        unsafe { std::env::set_var("VIRTUAL_ENV", venv.to_str().unwrap()) };
        let pwd = tmp.path().to_string_lossy().to_string();
        assert_eq!(read_python_version(&pwd).unwrap(), "3.12.0");
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
    }

    #[test]
    #[serial]
    fn no_version_returns_none() {
        let tmp = TempDir::new().unwrap();
        unsafe { std::env::remove_var("VIRTUAL_ENV") };
        let pwd = tmp.path().to_string_lossy().to_string();
        assert!(read_python_version(&pwd).is_none());
    }
}
