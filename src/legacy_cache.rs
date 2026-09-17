//! Retirement of the v1 rendered-prompt cache.
//!
//! Releases before this one wrote the rendered prompt to
//! `${XDG_RUNTIME_DIR:-/tmp}/chevron-<uid>/last-prompt`, staged through a
//! `last-prompt.tmp` sibling, and both the zsh init script and the pasted
//! instant-prompt block read that file back into `PROMPT`. On hosts without
//! a private runtime directory the parent lives in world-writable `/tmp`,
//! where another local user can precreate it and choose what the next shell
//! paints. Nothing reads the file any more. This module removes the known
//! entries so a still-pasted v1 block finds nothing to paint, and reports a
//! parent it refuses to touch so `chevron doctor` can surface it as a
//! security finding.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// The entries the v1 shell integration wrote: the rendered prompt and the
/// staging file it was renamed from.
const LEGACY_ENTRIES: [&CStr; 2] = [c"last-prompt", c"last-prompt.tmp"];

/// Outcome of one cleanup attempt. `chevron init` stays silent except for
/// [`Cleanup::UnsafeParent`]; `chevron doctor` reports every variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// The directory or both entries are already absent.
    Absent,
    /// At least one legacy entry was unlinked.
    Removed,
    /// The parent is not a plain directory owned by this user with mode
    /// 0700, or a symlink sits where the directory should be. Nothing was
    /// touched; the message names the path and the reason.
    UnsafeParent(String),
    /// The parent passed validation but an unlink failed.
    Failed(String),
}

/// `$XDG_RUNTIME_DIR/chevron-<uid>`, or `/tmp/chevron-<uid>` when the
/// variable is unset or empty. This mirrors the retired shell code exactly.
/// The old `CHEVRON_CACHE_FILE` override is deliberately ignored so the
/// cleanup can never be pointed at an arbitrary file.
#[must_use]
pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    base.join(format!("chevron-{}", effective_uid()))
}

/// Remove the legacy entries under [`dir`]. Idempotent and cheap enough to
/// run on every shell start.
#[must_use]
pub fn retire() -> Cleanup {
    retire_in(&dir())
}

/// Remove the legacy entries under `parent`, which must be a directory owned
/// by the effective user with no group or other permission bits.
///
/// The directory is opened once with `O_NOFOLLOW` and validated through its
/// descriptor; the entries are then unlinked relative to that descriptor.
/// Unlinking never follows a symlink, never opens the payload and never
/// recurses, so the worst case is removing a link and leaving its target
/// intact.
#[must_use]
pub fn retire_in(parent: &Path) -> Cleanup {
    let handle = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
    {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Cleanup::Absent,
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Cleanup::UnsafeParent(format!(
                "{} is a symlink where a directory is expected",
                parent.display()
            ));
        }
        Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => {
            return Cleanup::UnsafeParent(format!("{} is not a directory", parent.display()));
        }
        Err(e) => return Cleanup::Failed(format!("open {}: {e}", parent.display())),
    };
    let meta = match handle.metadata() {
        Ok(m) => m,
        Err(e) => return Cleanup::Failed(format!("stat {}: {e}", parent.display())),
    };
    if !meta.is_dir() {
        return Cleanup::UnsafeParent(format!("{} is not a directory", parent.display()));
    }
    if meta.uid() != effective_uid() {
        return Cleanup::UnsafeParent(format!(
            "{} is owned by uid {}, not by you",
            parent.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Cleanup::UnsafeParent(format!(
            "{} is accessible to other users (mode {:04o})",
            parent.display(),
            meta.mode() & 0o7777
        ));
    }
    let mut removed = false;
    for name in LEGACY_ENTRIES {
        match unlink_at(&handle, name) {
            Ok(true) => removed = true,
            Ok(false) => {}
            Err(e) => {
                return Cleanup::Failed(format!(
                    "remove {}/{}: {e}",
                    parent.display(),
                    name.to_string_lossy()
                ));
            }
        }
    }
    if removed {
        Cleanup::Removed
    } else {
        Cleanup::Absent
    }
}

/// Unlink `name` relative to the open directory `dir`. `Ok(true)` when an
/// entry was removed, `Ok(false)` when there was none.
fn unlink_at(dir: &File, name: &CStr) -> io::Result<bool> {
    // SAFETY: `dir` holds an open directory descriptor for the duration of
    // the call and `name` is a NUL-terminated single path component.
    let rc = unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(err)
    }
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid() has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::TempDir;

    fn owned_dir(root: &Path, mode: u32) -> PathBuf {
        let dir = root.join("chevron-test");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
        dir
    }

    fn seed(dir: &Path) {
        fs::write(dir.join("last-prompt"), "/x\n%{poison%}\n").unwrap();
        fs::write(dir.join("last-prompt.tmp"), "partial").unwrap();
    }

    #[test]
    fn missing_directory_is_absent() {
        let root = TempDir::new().unwrap();
        assert_eq!(retire_in(&root.path().join("nope")), Cleanup::Absent);
    }

    #[test]
    fn removes_both_entries_and_is_idempotent() {
        let root = TempDir::new().unwrap();
        let dir = owned_dir(root.path(), 0o700);
        seed(&dir);
        assert_eq!(retire_in(&dir), Cleanup::Removed);
        assert!(!dir.join("last-prompt").exists());
        assert!(!dir.join("last-prompt.tmp").exists());
        assert!(dir.is_dir(), "the directory itself is left alone");
        assert_eq!(retire_in(&dir), Cleanup::Absent);
    }

    #[test]
    fn removes_a_symlinked_entry_and_leaves_its_target() {
        let root = TempDir::new().unwrap();
        let dir = owned_dir(root.path(), 0o700);
        let target = root.path().join("victim");
        fs::write(&target, "keep me").unwrap();
        symlink(&target, dir.join("last-prompt")).unwrap();
        assert_eq!(retire_in(&dir), Cleanup::Removed);
        assert!(fs::symlink_metadata(dir.join("last-prompt")).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep me");
    }

    #[test]
    fn refuses_a_parent_other_users_can_reach() {
        let root = TempDir::new().unwrap();
        let dir = owned_dir(root.path(), 0o755);
        seed(&dir);
        match retire_in(&dir) {
            Cleanup::UnsafeParent(reason) => {
                assert!(reason.contains("accessible to other users"), "{reason}");
            }
            other => panic!("expected UnsafeParent, got {other:?}"),
        }
        assert!(
            dir.join("last-prompt").exists(),
            "nothing inside is touched"
        );
    }

    #[test]
    fn refuses_a_symlinked_parent() {
        let root = TempDir::new().unwrap();
        let real = owned_dir(root.path(), 0o700);
        seed(&real);
        let link = root.path().join("chevron-link");
        symlink(&real, &link).unwrap();
        assert!(matches!(retire_in(&link), Cleanup::UnsafeParent(_)));
        assert!(real.join("last-prompt").exists());
    }

    #[test]
    fn refuses_a_parent_that_is_a_file() {
        let root = TempDir::new().unwrap();
        let file = root.path().join("chevron-file");
        fs::write(&file, "not a dir").unwrap();
        assert!(matches!(retire_in(&file), Cleanup::UnsafeParent(_)));
        assert_eq!(fs::read_to_string(&file).unwrap(), "not a dir");
    }

    #[test]
    fn never_removes_a_directory_entry() {
        let root = TempDir::new().unwrap();
        let dir = owned_dir(root.path(), 0o700);
        fs::create_dir(dir.join("last-prompt")).unwrap();
        fs::write(dir.join("last-prompt").join("inner"), "x").unwrap();
        assert!(matches!(retire_in(&dir), Cleanup::Failed(_)));
        assert!(dir.join("last-prompt").join("inner").exists());
    }

    #[test]
    #[serial]
    fn dir_follows_runtime_dir_then_tmp() {
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/test") };
        assert!(dir().starts_with("/run/user/test"));
        assert!(dir().to_string_lossy().contains("/chevron-"));
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "") };
        assert!(dir().starts_with("/tmp"));
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        assert!(dir().starts_with("/tmp"));
    }
}
