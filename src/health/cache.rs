use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Cache directory for health check results.
/// Honors `PLX_HEALTH_CACHE` (for tests), then `XDG_CACHE_HOME`, then `$HOME/.cache`.
fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PLX_HEALTH_CACHE") {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(p).join("plx").join("health");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".cache")
        .join("plx")
        .join("health")
}

/// Read the cached value for `key` if it exists and is younger than `ttl`.
pub fn read(key: &str, ttl: Duration) -> Option<String> {
    let path = cache_dir().join(key);
    let meta = fs::metadata(&path).ok()?;
    let mtime = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    if age > ttl {
        return None;
    }
    fs::read_to_string(&path).ok()
}

/// Write `value` to the cache under `key`. Best-effort; failures are silent.
pub fn write(key: &str, value: &str) {
    let dir = cache_dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = fs::write(dir.join(key), value);
}

#[cfg(test)]
mod tests {
    use super::{read, write};
    use serial_test::serial;
    use std::time::Duration;
    use tempfile::TempDir;

    fn with_cache<F: FnOnce()>(f: F) {
        let tmp = TempDir::new().unwrap();
        // SAFETY: tests are serialized; env var is the cache root override.
        unsafe { std::env::set_var("PLX_HEALTH_CACHE", tmp.path()) };
        f();
        unsafe { std::env::remove_var("PLX_HEALTH_CACHE") };
    }

    #[test]
    #[serial]
    fn write_then_read_roundtrip() {
        with_cache(|| {
            write("foo", "bar");
            let got = read("foo", Duration::from_secs(60));
            assert_eq!(got.as_deref(), Some("bar"));
        });
    }

    #[test]
    #[serial]
    fn read_missing_returns_none() {
        with_cache(|| {
            let got = read("nope", Duration::from_secs(60));
            assert!(got.is_none());
        });
    }

    #[test]
    #[serial]
    fn read_returns_none_when_zero_ttl() {
        with_cache(|| {
            write("foo", "bar");
            // Sleep 10ms then read with 1ms TTL: should be stale.
            std::thread::sleep(Duration::from_millis(10));
            let got = read("foo", Duration::from_millis(1));
            assert!(got.is_none());
        });
    }
}
