//! Filesystem locations the daemon uses for its socket, lockfile, pidfile,
//! and log file. Both the daemon and the client need these, so the lookups
//! live in one place and are derived from environment variables with
//! sensible cross-platform defaults.

use std::path::PathBuf;

const SOCKET_FILE: &str = "chevrond.sock";
const LOCK_FILE: &str = "chevrond.lock";
const PID_FILE: &str = "chevrond.pid";
const LOG_FILE: &str = "chevrond.log";

/// Directory holding the daemon's runtime files. Resolution order:
///   1. `CHEVRON_SOCKET_DIR` — for tests and explicit overrides.
///   2. `$XDG_RUNTIME_DIR/chevron/` — Linux convention when set.
///   3. `/tmp/chevrond-$UID/` — macOS default (no `XDG_RUNTIME_DIR`) and
///      Linux fallback.
#[must_use]
pub fn socket_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CHEVRON_SOCKET_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        let mut p = PathBuf::from(d);
        p.push("chevron");
        return p;
    }
    PathBuf::from(format!("/tmp/chevrond-{}", current_uid()))
}

#[must_use]
pub fn socket_path() -> PathBuf {
    socket_dir().join(SOCKET_FILE)
}

#[must_use]
pub fn lock_path() -> PathBuf {
    socket_dir().join(LOCK_FILE)
}

#[must_use]
pub fn pid_path() -> PathBuf {
    socket_dir().join(PID_FILE)
}

/// Log file location. Resolution order:
///   1. `CHEVRON_LOG_DIR/chevrond.log`
///   2. `$XDG_STATE_HOME/chevron/chevrond.log`
///   3. `$HOME/.local/state/chevron/chevrond.log`
///   4. `./chevrond.log` (last-resort fallback)
#[must_use]
pub fn log_path() -> PathBuf {
    let mut dir = if let Some(d) = std::env::var_os("CHEVRON_LOG_DIR") {
        PathBuf::from(d)
    } else if let Some(d) = std::env::var_os("XDG_STATE_HOME") {
        let mut p = PathBuf::from(d);
        p.push("chevron");
        p
    } else if let Some(h) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(h);
        p.push(".local");
        p.push("state");
        p.push("chevron");
        p
    } else {
        PathBuf::from(".")
    };
    dir.push(LOG_FILE);
    dir
}

fn current_uid() -> u32 {
    // SAFETY: getuid() is async-signal-safe and always succeeds.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn socket_dir_honours_override() {
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", "/tmp/test-override") };
        assert_eq!(socket_dir(), PathBuf::from("/tmp/test-override"));
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn socket_dir_uses_xdg_runtime_dir_when_no_override() {
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000") };
        assert_eq!(socket_dir(), PathBuf::from("/run/user/1000/chevron"));
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
    }

    #[test]
    #[serial]
    fn socket_dir_falls_back_to_tmp_with_uid() {
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
        unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
        let d = socket_dir();
        assert!(
            d.to_string_lossy().starts_with("/tmp/chevrond-"),
            "got {d:?}"
        );
    }

    #[test]
    #[serial]
    fn socket_path_joins_filename() {
        unsafe { std::env::set_var("CHEVRON_SOCKET_DIR", "/tmp/x") };
        assert_eq!(socket_path(), PathBuf::from("/tmp/x/chevrond.sock"));
        assert_eq!(lock_path(), PathBuf::from("/tmp/x/chevrond.lock"));
        assert_eq!(pid_path(), PathBuf::from("/tmp/x/chevrond.pid"));
        unsafe { std::env::remove_var("CHEVRON_SOCKET_DIR") };
    }

    #[test]
    #[serial]
    fn log_path_uses_log_dir_override() {
        unsafe { std::env::set_var("CHEVRON_LOG_DIR", "/var/log/x") };
        assert_eq!(log_path(), PathBuf::from("/var/log/x/chevrond.log"));
        unsafe { std::env::remove_var("CHEVRON_LOG_DIR") };
    }
}
