//! Bounded static discovery, never shell evaluation. Module files are evidence
//! of configuration, not proof that a conditional/custom loader executed them.
use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};

use super::ShellTarget;

const MAX_ENTRIES: usize = 128;
const MAX_FILE_BYTES: u64 = 64 * 1024;

pub(super) fn active_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

pub(super) fn files(home: &str, target: ShellTarget) -> Vec<(PathBuf, String)> {
    let mut queue: VecDeque<_> = target
        .candidates()
        .iter()
        .map(|p| (Path::new(home).join(p), 0))
        .collect();
    // Zsh configurations commonly use a custom module loader, as Pat's does.
    // Discover conventional module directories without interpreting that code.
    if matches!(target, ShellTarget::Zsh) {
        queue.push_back((Path::new(home).join(".zsh"), 0));
        queue.push_back((Path::new(home).join(".config/zsh"), 0));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut visited = 0;
    while let Some((path, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_ENTRIES {
            break;
        }
        if depth > 8 {
            continue;
        }
        let Ok(real) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(real) {
            continue;
        }
        let Ok(meta) = path.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let Ok(entries) = path.read_dir() else {
                continue;
            };
            let mut paths: Vec<_> = entries
                .take(MAX_ENTRIES)
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            paths.sort();
            for child in paths {
                if child.is_dir() || child.extension().is_some_and(|e| e == "zsh" || e == "sh") {
                    queue.push_back((child, depth + 1));
                }
            }
            continue;
        }
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut contents = String::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut contents)
            .is_err()
            || contents.len() as u64 > MAX_FILE_BYTES
        {
            continue;
        }
        for line in active_lines(&contents) {
            if let Some(source) = source_path(line, home) {
                queue.push_back((source, depth + 1));
            }
        }
        out.push((path, contents));
    }
    out
}

fn source_path(line: &str, home: &str) -> Option<PathBuf> {
    let rest = line
        .strip_prefix("source ")
        .or_else(|| line.strip_prefix(". "))?
        .trim();
    let path = if let Some(quote @ ('\'' | '"')) = rest.chars().next() {
        rest.get(1..)?.split(quote).next()?
    } else {
        rest.split_whitespace().next()?.trim_end_matches(';')
    };
    let expanded = if let Some(suffix) = path
        .strip_prefix("$HOME/")
        .or_else(|| path.strip_prefix("${HOME}/"))
        .or_else(|| path.strip_prefix("~/"))
    {
        Path::new(home).join(suffix)
    } else {
        PathBuf::from(path)
    };
    // Literal absolute paths only. Relative paths resolve against the shell's
    // runtime cwd, which doctor cannot infer from the config file location.
    (expanded.is_absolute()
        && !path.contains(['`', '(', ')'])
        && !path
            .replace("${HOME}", "")
            .replace("$HOME", "")
            .contains('$'))
    .then_some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_literal_sources_and_stops_at_cycles_and_commands() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        std::fs::write(
            tmp.path().join(".zshrc"),
            "source \"$HOME/prompt.sh\"\nsource \"$(touch BAD)\"\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("prompt.sh"),
            ". ${HOME}/.zshrc\neval \"$(chevron init zsh)\"\n",
        )
        .unwrap();
        let found = files(home, ShellTarget::Zsh);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|(_, text)| text.contains("chevron init")));
        assert_eq!(source_path("source \"$(touch BAD)\"", home), None);
        assert_eq!(source_path("source relative.sh", home), None);
    }

    #[test]
    fn handles_nix_style_symlinked_module_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tempfile::tempdir().unwrap();
        std::fs::write(
            modules.path().join("prompt.zsh"),
            "eval \"$(chevron init zsh)\"",
        )
        .unwrap();
        std::os::unix::fs::symlink(modules.path(), tmp.path().join(".zsh")).unwrap();
        let found = files(tmp.path().to_str().unwrap(), ShellTarget::Zsh);
        assert_eq!(found.len(), 1);
        assert!(found[0].0.ends_with(".zsh/prompt.zsh"));
    }
}
