//! CLI and environment parsing for `chevron weather`.
//!
//! We don't pull in clap — the flag surface is small and the rest of chevron
//! parses `env::args()` by hand. This module also layers env vars on top of
//! CLI defaults so callers can see the CLI > env > TOML > built-in precedence
//! in one place.

/// Parsed weather options.
///
/// Default values here are the "built-in defaults" in the CLI > env > TOML >
/// default chain. The `*_was_defaulted` flags let the higher layer merge TOML
/// only when neither CLI nor env set the value.
///
/// `clippy::struct_excessive_bools` is allowed because the `*_was_defaulted`
/// flags are a deliberate design to thread "did this come from CLI/env or
/// from the built-in default" through to [`super::merge_options`] without
/// introducing a second wrapper type.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct Options {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub location_cmd: Option<String>,
    pub api_key: Option<String>,

    pub provider: String,
    pub provider_was_defaulted: bool,

    pub units: String,
    pub units_was_defaulted: bool,

    pub cache_ttl_min: u64,
    pub cache_ttl_was_defaulted: bool,

    pub show_city: bool,
    pub show_city_was_defaulted: bool,

    pub show_icon: bool,
    pub show_icon_was_defaulted: bool,

    pub use_nerd_font: bool,
    pub use_nerd_font_was_defaulted: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            lat: None,
            lon: None,
            location_cmd: None,
            api_key: None,
            provider: String::from("openmeteo"),
            provider_was_defaulted: true,
            units: String::from("metric"),
            units_was_defaulted: true,
            cache_ttl_min: 15,
            cache_ttl_was_defaulted: true,
            show_city: true,
            show_city_was_defaulted: true,
            show_icon: true,
            show_icon_was_defaulted: true,
            use_nerd_font: false,
            use_nerd_font_was_defaulted: true,
        }
    }
}

impl Options {
    /// Cache TTL in seconds.
    #[must_use]
    pub fn cache_ttl_secs(&self) -> u64 {
        self.cache_ttl_min.saturating_mul(60)
    }
}

/// Parse argv (the slice after `chevron weather`), layering env vars on top of
/// built-in defaults before flags override.
///
/// Returns `Err` only when a flag is malformed (e.g. `--lat abc`). The caller
/// ([`super::run`]) treats any error as a soft failure and continues with
/// defaults, so tmux never sees the error.
pub fn parse(argv: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    apply_env(&mut opts)?;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        match arg {
            "--lat" => {
                let v = take_value(argv, &mut i, arg)?;
                opts.lat = Some(parse_f64(&v, arg)?);
            }
            "--lon" => {
                let v = take_value(argv, &mut i, arg)?;
                opts.lon = Some(parse_f64(&v, arg)?);
            }
            "--location-cmd" => {
                opts.location_cmd = Some(take_value(argv, &mut i, arg)?);
            }
            "--provider" => {
                opts.provider = take_value(argv, &mut i, arg)?;
                opts.provider_was_defaulted = false;
            }
            "--api-key" => {
                opts.api_key = Some(take_value(argv, &mut i, arg)?);
            }
            "--units" => {
                opts.units = normalize_units(&take_value(argv, &mut i, arg)?);
                opts.units_was_defaulted = false;
            }
            "--cache-ttl" => {
                let v = take_value(argv, &mut i, arg)?;
                opts.cache_ttl_min = parse_u64(&v, arg)?;
                opts.cache_ttl_was_defaulted = false;
            }
            "--no-show-city" => {
                opts.show_city = false;
                opts.show_city_was_defaulted = false;
            }
            "--show-city" => {
                // Accept the positive form too — the default is true, but this
                // makes the spec's `#(chevron weather --show-city)` explicit.
                opts.show_city = true;
                opts.show_city_was_defaulted = false;
            }
            "--no-show-icon" => {
                opts.show_icon = false;
                opts.show_icon_was_defaulted = false;
            }
            "--show-icon" => {
                opts.show_icon = true;
                opts.show_icon_was_defaulted = false;
            }
            "--use-nerd-font" => {
                opts.use_nerd_font = true;
                opts.use_nerd_font_was_defaulted = false;
            }
            "--no-use-nerd-font" => {
                opts.use_nerd_font = false;
                opts.use_nerd_font_was_defaulted = false;
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

fn apply_env(opts: &mut Options) -> Result<(), String> {
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_LAT")
        && !v.is_empty()
    {
        opts.lat = Some(parse_f64(&v, "CHEVRON_WEATHER_LAT")?);
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_LON")
        && !v.is_empty()
    {
        opts.lon = Some(parse_f64(&v, "CHEVRON_WEATHER_LON")?);
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_API_KEY")
        && !v.is_empty()
    {
        opts.api_key = Some(v);
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_PROVIDER")
        && !v.is_empty()
    {
        opts.provider = v;
        opts.provider_was_defaulted = false;
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_UNITS")
        && !v.is_empty()
    {
        opts.units = normalize_units(&v);
        opts.units_was_defaulted = false;
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_CACHE_TTL")
        && !v.is_empty()
    {
        opts.cache_ttl_min = parse_u64(&v, "CHEVRON_WEATHER_CACHE_TTL")?;
        opts.cache_ttl_was_defaulted = false;
    }
    if let Ok(v) = std::env::var("CHEVRON_WEATHER_LOCATION_CMD")
        && !v.is_empty()
    {
        opts.location_cmd = Some(v);
    }
    Ok(())
}

fn take_value(argv: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    *i += 1;
    argv.get(*i)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_f64(s: &str, ctx: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .map_err(|_| format!("{ctx}: not a number: {s}"))
        .and_then(|f| {
            if f.is_finite() {
                Ok(f)
            } else {
                Err(format!("{ctx}: must be finite"))
            }
        })
}

fn parse_u64(s: &str, ctx: &str) -> Result<u64, String> {
    s.parse::<u64>()
        .map_err(|_| format!("{ctx}: not a non-negative integer: {s}"))
}

fn normalize_units(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "imperial" | "us" | "f" => "imperial".to_string(),
        _ => "metric".to_string(),
    }
}

pub const fn help_text() -> &'static str {
    "\
chevron weather — fetch current conditions and print a one-line summary

USAGE:
    chevron weather [FLAGS]

LOCATION (optional; IP geolocation via ifconfig.co is used when none given):
        --lat FLOAT            Latitude
        --lon FLOAT            Longitude
        --location-cmd CMD     Shell command; its stdout must be \"lat|lon\"

PROVIDER:
        --provider NAME        openmeteo (default) or openweather
        --api-key KEY          Required for openweather

FORMAT:
        --units UNITS          metric (default) or imperial
        --cache-ttl MIN        Cache TTL in minutes (default 15)
        --no-show-city         Hide \"City, CC\" prefix (default: show)
        --no-show-icon         Hide weather icon (default: show)
        --use-nerd-font        Use Nerd Font glyphs for icons

MISC:
    -h, --help                 Show this help

ENVIRONMENT:
    CHEVRON_WEATHER_LAT, CHEVRON_WEATHER_LON, CHEVRON_WEATHER_API_KEY,
    CHEVRON_WEATHER_PROVIDER, CHEVRON_WEATHER_UNITS, CHEVRON_WEATHER_CACHE_TTL,
    CHEVRON_WEATHER_LOCATION_CMD, CHEVRON_WEATHER_DEBUG=1 (log errors to stderr)

NOTES:
    Designed for tmux status-right. Always exits 0 and prints an empty
    string on any error (network timeout, parse failure, geocode miss).
    Every HTTP call is capped at 3 seconds. On timeout, falls back to
    the previously cached value if present.
"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_env() {
        for k in [
            "CHEVRON_WEATHER_LAT",
            "CHEVRON_WEATHER_LON",
            "CHEVRON_WEATHER_API_KEY",
            "CHEVRON_WEATHER_PROVIDER",
            "CHEVRON_WEATHER_UNITS",
            "CHEVRON_WEATHER_CACHE_TTL",
            "CHEVRON_WEATHER_LOCATION_CMD",
        ] {
            // SAFETY: test-only env mutation, guarded by serial_test in callers
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    #[serial_test::serial]
    fn default_options() {
        clear_env();
        let opts = parse(&[]).unwrap();
        assert_eq!(opts.provider, "openmeteo");
        assert_eq!(opts.units, "metric");
        assert_eq!(opts.cache_ttl_min, 15);
        assert!(opts.show_city);
        assert!(opts.show_icon);
        assert!(!opts.use_nerd_font);
        assert!(opts.lat.is_none());
        assert!(opts.lon.is_none());
    }

    #[test]
    #[serial_test::serial]
    fn cli_flags_parsed() {
        clear_env();
        let argv: Vec<String> = vec![
            "--lat",
            "47.13",
            "--lon",
            "-122.16",
            "--units",
            "imperial",
            "--no-show-city",
            "--use-nerd-font",
            "--cache-ttl",
            "60",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let opts = parse(&argv).unwrap();
        assert!((opts.lat.unwrap() - 47.13).abs() < 1e-9);
        assert!((opts.lon.unwrap() - -122.16).abs() < 1e-9);
        assert_eq!(opts.units, "imperial");
        assert!(!opts.show_city);
        assert!(opts.use_nerd_font);
        assert_eq!(opts.cache_ttl_min, 60);
    }

    #[test]
    #[serial_test::serial]
    fn env_vars_applied() {
        clear_env();
        // SAFETY: test-only
        unsafe {
            std::env::set_var("CHEVRON_WEATHER_LAT", "40.0");
            std::env::set_var("CHEVRON_WEATHER_LON", "-74.0");
            std::env::set_var("CHEVRON_WEATHER_UNITS", "imperial");
        }
        let opts = parse(&[]).unwrap();
        assert_eq!(opts.lat, Some(40.0));
        assert_eq!(opts.lon, Some(-74.0));
        assert_eq!(opts.units, "imperial");
        clear_env();
    }

    #[test]
    #[serial_test::serial]
    fn cli_overrides_env() {
        clear_env();
        // SAFETY: test-only
        unsafe { std::env::set_var("CHEVRON_WEATHER_UNITS", "metric") };
        let argv: Vec<String> = ["--units", "imperial"]
            .iter()
            .copied()
            .map(String::from)
            .collect();
        let opts = parse(&argv).unwrap();
        assert_eq!(opts.units, "imperial");
        clear_env();
    }

    #[test]
    #[serial_test::serial]
    fn unknown_flag_is_err() {
        clear_env();
        let argv: Vec<String> = ["--bogus"].iter().copied().map(String::from).collect();
        assert!(parse(&argv).is_err());
    }

    #[test]
    #[serial_test::serial]
    fn bad_number_is_err() {
        clear_env();
        let argv: Vec<String> = ["--lat", "nope"]
            .iter()
            .copied()
            .map(String::from)
            .collect();
        assert!(parse(&argv).is_err());
    }

    #[test]
    fn help_text_lists_all_flags() {
        let h = help_text();
        for flag in [
            "--lat",
            "--lon",
            "--location-cmd",
            "--provider",
            "--api-key",
            "--units",
            "--cache-ttl",
            "--no-show-city",
            "--no-show-icon",
            "--use-nerd-font",
        ] {
            assert!(h.contains(flag), "help missing {flag}");
        }
    }
}
