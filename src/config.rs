use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub segments: SegmentsConfig,
    /// Per-segment config blocks: `[segment.git]`, `[segment.path]`, etc.
    #[serde(default)]
    pub segment: HashMap<String, SegmentConfig>,
    /// `[weather]` TOML section (optional). All fields individually optional.
    /// Consumed only when the `weather` cargo feature is enabled.
    #[serde(default)]
    #[cfg_attr(not(feature = "weather"), allow(dead_code))]
    pub weather: WeatherConfig,
    /// `[health]` TOML section (optional). All fields individually optional.
    #[serde(default)]
    pub health: HealthConfig,
    /// `[shell]` TOML section — toggles consumed by `chevron init <shell>`
    /// when emitting the shell init script. Each field's value becomes the
    /// default for the corresponding CHEVRON_* env var; explicit env-var
    /// exports still win (env > config > default).
    #[serde(default)]
    pub shell: ShellConfig,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SegmentsConfig {
    /// Ordered list of segment names. Empty means use the default order.
    pub order: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SegmentConfig {
    /// Whether this segment is enabled. `None` means use the default (true).
    pub enabled: Option<bool>,
    /// Foreground color override (256-color).
    pub fg: Option<u8>,
    /// Background color override (256-color).
    pub bg: Option<u8>,
    /// Shell command to run (`custom_command` segment).
    pub command: Option<String>,
    /// Cache TTL in seconds (`custom_command` segment). Default: 30.
    pub cache_secs: Option<u64>,
    /// Command timeout in milliseconds (`custom_command` segment). Default: 500.
    pub timeout_ms: Option<u64>,
    /// `[segment.path].repo_relative` — render paths relative to the
    /// containing git repo's parent dir when inside a repo. Default: true.
    /// Env override: `CHEVRON_REPO_RELATIVE_PATH=0` (env > config > default).
    pub repo_relative: Option<bool>,
}

/// `[shell]` block — defaults for the env vars the init scripts read.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
// Five booleans plus a numeric — matches the surface of init-time toggles.
// Splitting them into sub-structs adds indirection without clarity.
#[allow(clippy::struct_excessive_bools)]
pub struct ShellConfig {
    /// `CHEVRON_OSC133` default. Emit OSC 133 prompt/output markers.
    pub osc133: bool,
    /// `CHEVRON_TRANSIENT` default. Collapse the previous prompt to a single chevron.
    pub transient: bool,
    /// `CHEVRON_TRANSIENT_DURATION_MS` default. Threshold above which a
    /// dim "duration" tag is printed below command output.
    pub transient_duration_ms: u64,
    /// `CHEVRON_ASYNC` default. Async prompt-render fast path (zsh only).
    pub async_render: bool,
    /// `CHEVRON_HISTORY` default. Record command lifecycle to chevrond.
    pub history: bool,
    /// `CHEVRON_LIVE` default. Live prompt refresh on filesystem changes.
    pub live: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            osc133: true,
            transient: true,
            transient_duration_ms: 2000,
            async_render: false,
            history: true,
            live: false,
        }
    }
}

/// `[weather]` TOML section. All fields optional; CLI flags and env vars
/// override anything set here. CLI > env > TOML > built-in defaults.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct WeatherConfig {
    /// `"openmeteo"` (default) or `"openweather"`.
    pub provider: Option<String>,
    /// API key (required for `openweather`).
    pub api_key: Option<String>,
    /// `"metric"` (default) or `"imperial"`.
    pub units: Option<String>,
    /// Cache TTL in minutes. Default: 15.
    pub cache_ttl: Option<u64>,
    /// Show `"City, CC"` prefix. Default: true.
    pub show_city: Option<bool>,
    /// Show weather icon. Default: true.
    pub show_icon: Option<bool>,
    /// Use Nerd Font glyphs instead of plain Unicode. Default: false.
    pub use_nerd_font: Option<bool>,
    /// Fixed latitude override.
    pub lat: Option<f64>,
    /// Fixed longitude override.
    pub lon: Option<f64>,
    /// Shell command that prints `"lat|lon"` on stdout.
    pub location_cmd: Option<String>,
}

/// `[health]` TOML section. All fields optional; missing fields use compiled defaults.
#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct HealthConfig {
    /// Threshold overrides per check.
    pub thresholds: Thresholds,
    /// Network reachability check target.
    pub network: NetworkConfig,
    /// Checks to skip in the full report (single-check mode still runs them).
    pub disabled: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Thresholds {
    /// Per-core load average warn threshold.
    pub load: f64,
    /// Memory usage % warn threshold.
    pub memory_warn: f64,
    /// Memory usage % critical threshold.
    pub memory_critical: f64,
    /// Disk usage % warn threshold.
    pub disk_warn: f64,
    /// Disk usage % critical threshold.
    pub disk_critical: f64,
    /// CPU temperature (°C) warn threshold.
    pub cpu_temp_warn: f64,
    /// CPU temperature (°C) critical threshold.
    pub cpu_temp_critical: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            load: 1.0,
            memory_warn: 90.0,
            memory_critical: 95.0,
            disk_warn: 90.0,
            disk_critical: 95.0,
            cpu_temp_warn: 80.0,
            cpu_temp_critical: 90.0,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct NetworkConfig {
    pub host: String,
    pub port: u16,
    pub timeout_ms: u64,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            host: "1.1.1.1".to_string(),
            port: 53,
            timeout_ms: 500,
        }
    }
}

impl Config {
    /// Load config from disk. Returns defaults if the file is missing.
    /// Prints to stderr and returns defaults if the file exists but is invalid.
    #[must_use]
    pub fn load() -> Self {
        let path = config_path();
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("chevron: invalid config at {}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Returns whether a segment is enabled. Segments are enabled by default
    /// unless explicitly set to `enabled = false`.
    #[must_use]
    pub fn segment_enabled(&self, name: &str) -> bool {
        self.segment
            .get(name)
            .and_then(|s| s.enabled)
            .unwrap_or(true)
    }
}

fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("CHEVRON_CONFIG") {
        return PathBuf::from(path);
    }
    let base = std::env::var("XDG_CONFIG_HOME").map_or_else(
        |_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| String::from("."));
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
    base.join("chevron").join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn default_config_has_empty_order() {
        let cfg = Config::default();
        assert!(cfg.segments.order.is_empty());
        assert!(cfg.segment.is_empty());
    }

    #[test]
    fn parse_minimal_config() {
        let toml = "";
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.segments.order.is_empty());
    }

    #[test]
    fn parse_custom_order() {
        let toml = r#"
[segments]
order = ["path", "git", "character"]
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.segments.order, vec!["path", "git", "character"]);
    }

    #[test]
    fn parse_segment_enabled() {
        let toml = r"
[segment.hostname]
enabled = false

[segment.git]
enabled = true
";
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(!cfg.segment_enabled("hostname"));
        assert!(cfg.segment_enabled("git"));
    }

    #[test]
    fn segment_enabled_defaults_to_true() {
        let cfg = Config::default();
        assert!(cfg.segment_enabled("anything"));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let toml = r"
[segment.path]
enabled = true
max_dir_size = 15
fg = 200
";
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.segment_enabled("path"));
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact comparison of Default values
    fn health_defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.health.thresholds.load, 1.0);
        assert_eq!(cfg.health.thresholds.memory_warn, 90.0);
        assert_eq!(cfg.health.thresholds.memory_critical, 95.0);
        assert_eq!(cfg.health.thresholds.cpu_temp_warn, 80.0);
        assert_eq!(cfg.health.network.host, "1.1.1.1");
        assert_eq!(cfg.health.network.port, 53);
        assert_eq!(cfg.health.network.timeout_ms, 500);
        assert!(cfg.health.disabled.is_empty());
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact comparison of parsed-then-checked literals
    fn health_thresholds_can_be_partially_overridden() {
        let toml = r"
[health.thresholds]
memory_warn = 75
";
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.health.thresholds.memory_warn, 75.0);
        // Unset fields keep their defaults
        assert_eq!(cfg.health.thresholds.memory_critical, 95.0);
        assert_eq!(cfg.health.thresholds.load, 1.0);
    }

    #[test]
    fn health_disabled_list_parses() {
        let toml = r#"
[health]
disabled = ["network", "software_updates"]
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.health.disabled, vec!["network", "software_updates"]);
    }

    #[test]
    fn health_network_override_partial() {
        let toml = r#"
[health.network]
host = "8.8.8.8"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.health.network.host, "8.8.8.8");
        assert_eq!(cfg.health.network.port, 53); // default
        assert_eq!(cfg.health.network.timeout_ms, 500); // default
    }

    #[test]
    #[serial]
    fn load_missing_file_returns_default() {
        // Serial: Config::load() reads CHEVRON_CONFIG, racing with any test
        // that sets it. Same pattern as the NODE_VERSION incident.
        unsafe { std::env::set_var("CHEVRON_CONFIG", "/tmp/chevron-nonexistent-test.toml") };
        let cfg = Config::load();
        assert!(cfg.segments.order.is_empty());
        unsafe { std::env::remove_var("CHEVRON_CONFIG") };
    }
}
