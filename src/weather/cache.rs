//! Weather render cache.
//!
//! The cache file is JSON at `$XDG_CACHE_HOME/chevron/weather.json`, falling back
//! to `~/.cache/chevron/weather.json`. We store one entry per cache key — a tuple
//! of (provider, rounded lat, rounded lon, units) — so callers who switch
//! between e.g. metric and imperial during a debug session don't thrash.
//!
//! Why JSON rather than atime on the file: we want to cache the rendered line
//! itself, so a tmux status bar hit is a single read-and-print with no render
//! work. And we want multiple entries per file (per-units, per-provider).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Cache key: stringified so it's a valid JSON object key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

impl Key {
    #[must_use]
    pub fn new(provider: &str, lat: f64, lon: f64, units: &str) -> Self {
        // Round to 2 decimal places to collapse neighboring requests and
        // avoid cache thrash on sub-100m jitter from IP geolocation.
        Self(format!("{provider}:{lat:.2}:{lon:.2}:{units}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One cached rendered line, plus its fetch timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub fetched_at: u64,
    pub rendered: String,
}

impl Hit {
    #[must_use]
    pub fn new(fetched_at: u64, rendered: String) -> Self {
        Self {
            fetched_at,
            rendered,
        }
    }

    /// Is this entry older than `ttl_secs`?
    #[must_use]
    pub fn is_stale(&self, now: u64, ttl_secs: u64) -> bool {
        now.saturating_sub(self.fetched_at) >= ttl_secs
    }
}

/// On-disk cache file contents. Keyed by [`Key::as_str`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub hits: HashMap<String, Hit>,
}

impl Entry {
    #[must_use]
    pub fn lookup(&self, key: &Key) -> Option<&Hit> {
        self.hits.get(key.as_str())
    }

    pub fn insert(&mut self, key: Key, hit: Hit) {
        self.hits.insert(key.0, hit);
    }
}

/// Canonical cache path. Returns `$XDG_CACHE_HOME/chevron/weather.json`, or
/// `$HOME/.cache/chevron/weather.json`, or `./.chevron-weather.json` as a last resort.
#[must_use]
pub fn default_path() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return PathBuf::from(x).join("chevron").join("weather.json");
    }
    if let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return PathBuf::from(h)
            .join(".cache")
            .join("chevron")
            .join("weather.json");
    }
    PathBuf::from(".chevron-weather.json")
}

/// Read the cache file. Returns `None` if the file doesn't exist or can't be
/// parsed — the caller should treat that as a cache miss, not an error.
#[must_use]
pub fn read(path: &Path) -> Option<Entry> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Write the cache atomically-ish: create parent dirs first, then write.
/// Errors are non-fatal — caller ignores.
pub fn write(path: &Path, entry: &Entry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string(entry).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    fn key_rounds_to_2dp() {
        let a = Key::new("openmeteo", 47.1234, -122.5678, "metric");
        let b = Key::new("openmeteo", 47.1249, -122.5699, "metric");
        // Both round to 47.12, -122.57.
        assert_eq!(a, b);
    }

    #[test]
    fn key_differs_by_provider() {
        let a = Key::new("openmeteo", 1.0, 2.0, "metric");
        let b = Key::new("openweather", 1.0, 2.0, "metric");
        assert_ne!(a, b);
    }

    #[test]
    fn key_differs_by_units() {
        let a = Key::new("openmeteo", 1.0, 2.0, "metric");
        let b = Key::new("openmeteo", 1.0, 2.0, "imperial");
        assert_ne!(a, b);
    }

    #[test]
    fn hit_stale_is_time_based() {
        let h = Hit::new(100, "x".into());
        assert!(!h.is_stale(100, 60));
        assert!(!h.is_stale(159, 60));
        assert!(h.is_stale(160, 60));
        assert!(h.is_stale(1000, 60));
    }

    #[test]
    fn hit_stale_with_ttl_zero_is_always_stale() {
        let h = Hit::new(100, "x".into());
        assert!(h.is_stale(100, 0));
        assert!(h.is_stale(101, 0));
    }

    #[test]
    fn roundtrip_write_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("chevron").join("weather.json");

        let mut entry = Entry::default();
        entry.insert(
            Key::new("openmeteo", 47.13, -122.16, "imperial"),
            Hit::new(1_700_000_000, "Seattle 54F".into()),
        );

        write(&path, &entry).unwrap();

        let loaded = read(&path).unwrap();
        let key = Key::new("openmeteo", 47.13, -122.16, "imperial");
        let hit = loaded.lookup(&key).unwrap();
        assert_eq!(hit.rendered, "Seattle 54F");
        assert_eq!(hit.fetched_at, 1_700_000_000);
    }

    #[test]
    fn missing_file_read_is_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        assert!(read(&path).is_none());
    }

    #[test]
    fn corrupt_file_read_is_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("corrupt.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(read(&path).is_none());
    }

    #[test]
    fn entry_lookup_miss_is_none() {
        let entry = Entry::default();
        let key = Key::new("openmeteo", 47.13, -122.16, "imperial");
        assert!(entry.lookup(&key).is_none());
    }

    #[test]
    #[serial]
    fn default_path_prefers_xdg() {
        // Serial: XDG_CACHE_HOME is also read by health::cache::cache_dir;
        // racing across modules could yield false negatives in either.
        // SAFETY: test-only
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", "/tmp/xdg-cache-test-chevron");
        }
        let p = default_path();
        assert!(
            p.to_string_lossy()
                .contains("xdg-cache-test-chevron/chevron/weather.json")
        );
        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
    }
}
