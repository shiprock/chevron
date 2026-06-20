//! Safe write: back up an existing file before overwriting, write the new
//! content atomically (.tmp + rename) so a partial write can never leave
//! the user's config in a corrupt state.

use std::io;
use std::path::{Path, PathBuf};

pub struct WriteResult {
    /// Path of the backup file, if the destination already existed.
    pub backed_up: Option<PathBuf>,
    /// Path that was written (== input `path`).
    pub written: PathBuf,
}

/// Write `contents` to `path`. If `path` already exists, copy it to
/// `<path>.bak` first (overwriting any prior .bak). The write itself is
/// atomic: contents go to `<path>.tmp` and are renamed into place.
pub fn write_with_backup(path: &Path, contents: &str) -> io::Result<WriteResult> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let backed_up = if path.exists() {
        let bak = backup_path(path);
        std::fs::copy(path, &bak)?;
        Some(bak)
    } else {
        None
    };

    // Write atomic: tmp + rename.
    let tmp = tmp_path(path);
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;

    Ok(WriteResult {
        backed_up,
        written: path.to_path_buf(),
    })
}

fn backup_path(path: &Path) -> PathBuf {
    // Append `.bak` to the full filename so the backup is obvious in
    // listings (`config.toml.bak`) rather than replacing the extension.
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".bak");
    path.with_file_name(name)
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_to_new_path_no_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        let res = write_with_backup(&path, "hello").unwrap();
        assert!(res.backed_up.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn backs_up_existing_file_then_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "original").unwrap();

        let res = write_with_backup(&path, "updated").unwrap();
        let bak = res.backed_up.expect("expected a backup");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "original");
        assert_eq!(
            bak.extension().and_then(|s| s.to_str()),
            Some("bak"),
            "backup filename must end in .bak"
        );
    }

    #[test]
    fn second_write_overwrites_the_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "v1").unwrap();
        write_with_backup(&path, "v2").unwrap();
        let res = write_with_backup(&path, "v3").unwrap();
        let bak = res.backed_up.unwrap();
        // The .bak now contains v2, not v1.
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v3");
    }

    #[test]
    fn creates_parent_directories_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/c/config.toml");
        write_with_backup(&path, "x").unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    #[test]
    fn backup_path_appends_dot_bak() {
        let p = Path::new("/tmp/config.toml");
        assert_eq!(backup_path(p), PathBuf::from("/tmp/config.toml.bak"));
    }

    #[test]
    fn tmp_path_appends_dot_tmp() {
        let p = Path::new("/tmp/config.toml");
        assert_eq!(tmp_path(p), PathBuf::from("/tmp/config.toml.tmp"));
    }
}
