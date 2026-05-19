//! `chevron weather` subcommand.
//!
//! # Design contract
//!
//! This command is invoked potentially every second from tmux `status-right`.
//! The function [`run`] MUST:
//!
//! * Exit 0 on every failure path. Caller reads stdout straight into a status
//!   bar, so any non-zero exit or panic splats onto the user's screen.
//! * Print only the rendered weather line (or an empty string) to stdout. All
//!   diagnostics go to stderr.
//! * Hit the cache first — single-digit milliseconds when fresh. Only touch the
//!   network on miss or stale entry.
//! * Cap every HTTP fetch at 3 seconds. Fall back to the stale cached render
//!   (if any) or the empty string on timeout.
//!
//! # Precedence
//!
//! `CLI flag` > `CHEVRON_WEATHER_*` env var > `[weather]` TOML > built-in default.
//!
//! # No CLIMA_* env vars
//!
//! We deliberately do NOT read `CLIMA_*` — that is one user's private vendored
//! naming. Use the `CHEVRON_WEATHER_*` namespace exclusively.

mod args;
mod cache;
mod format;
mod location;
mod openmeteo;
mod openweather;
mod providers;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{Config, WeatherConfig};

pub use args::Options;

/// Entry point for `chevron weather <flags>`.
///
/// Error-silent: always exits 0 and always prints to stdout exactly one of:
///
/// * a rendered line (e.g. `"Seattle, US \u{f00d} 54\u{b0}F"`)
/// * an empty string
///
/// Errors are reported only on stderr (and only when `CHEVRON_WEATHER_DEBUG=1`).
pub fn run(argv: &[String]) {
    // `--help` / `-h` bypasses everything and prints a manual help string.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", args::help_text());
        return;
    }

    // Parse CLI. Parse errors are not fatal — we fall back to defaults,
    // because splatting a parse error onto the tmux bar is worse than a
    // silent no-op.
    let cli = match args::parse(argv) {
        Ok(opts) => opts,
        Err(e) => {
            debug(&format!("arg parse: {e}"));
            Options::default()
        }
    };

    // Merge CLI > env > TOML > default.
    let cfg = Config::load().weather;
    let opts = merge_options(&cli, &cfg);

    if let Err(e) = run_inner(&opts) {
        debug(&e);
        // Error-silent: empty stdout.
    }
}

fn run_inner(opts: &Options) -> Result<(), String> {
    // 1. Resolve location. This may itself hit the network (IP geolocation).
    let loc = location::resolve(opts)?;

    let cache_path = cache::default_path();
    let provider_name = opts.provider.as_str();
    let key = cache::Key::new(provider_name, loc.lat, loc.lon, &opts.units);

    // 2. Cache lookup. If fresh, write rendered line and return.
    let now = now_secs();
    let ttl = opts.cache_ttl_secs();
    if let Some(entry) = cache::read(&cache_path)
        && let Some(hit) = entry.lookup(&key)
        && !hit.is_stale(now, ttl)
    {
        print!("{}", hit.rendered);
        return Ok(());
    }

    // 3. Cache miss or stale. Try the network.
    let provider = providers::resolve(provider_name, opts.api_key.as_deref())?;
    let fetch_result = provider.fetch(loc.lat, loc.lon, &opts.units);

    match fetch_result {
        Ok(data) => {
            // Merge city from geolocation if the provider didn't supply one.
            let city = data.city.clone().or_else(|| loc.city.clone());
            let country = data.country.clone().or_else(|| loc.country.clone());
            let rendered = format::render_line(&data, city.as_deref(), country.as_deref(), opts);

            // Best-effort cache write.
            let mut entry = cache::read(&cache_path).unwrap_or_default();
            entry.insert(key, cache::Hit::new(now, rendered.clone()));
            let _ = cache::write(&cache_path, &entry);

            print!("{rendered}");
            Ok(())
        }
        Err(e) => {
            // Network failed. Fall back to stale cached render if present.
            debug(&format!("provider fetch: {e}"));
            let entry = cache::read(&cache_path).unwrap_or_default();
            if let Some(hit) = entry.lookup(&key) {
                print!("{}", hit.rendered);
            }
            let _ = e; // explicit: failure is non-fatal and not propagated
            Ok(())
        }
    }
}

/// Merge CLI opts with TOML config. CLI values already contain env overrides
/// via [`args::parse`]; TOML is the bottom layer below those.
fn merge_options(cli: &Options, toml_cfg: &WeatherConfig) -> Options {
    let mut out = cli.clone();

    if out.lat.is_none() {
        out.lat = toml_cfg.lat;
    }
    if out.lon.is_none() {
        out.lon = toml_cfg.lon;
    }
    if out.location_cmd.is_none() {
        out.location_cmd.clone_from(&toml_cfg.location_cmd);
    }
    if out.api_key.is_none() {
        out.api_key.clone_from(&toml_cfg.api_key);
    }
    if out.provider_was_defaulted
        && let Some(p) = &toml_cfg.provider
    {
        out.provider.clone_from(p);
        out.provider_was_defaulted = false;
    }
    if out.units_was_defaulted
        && let Some(u) = &toml_cfg.units
    {
        out.units.clone_from(u);
        out.units_was_defaulted = false;
    }
    if out.cache_ttl_was_defaulted
        && let Some(t) = toml_cfg.cache_ttl
    {
        out.cache_ttl_min = t;
        out.cache_ttl_was_defaulted = false;
    }
    if out.show_city_was_defaulted
        && let Some(v) = toml_cfg.show_city
    {
        out.show_city = v;
        out.show_city_was_defaulted = false;
    }
    if out.show_icon_was_defaulted
        && let Some(v) = toml_cfg.show_icon
    {
        out.show_icon = v;
        out.show_icon_was_defaulted = false;
    }
    if out.use_nerd_font_was_defaulted
        && let Some(v) = toml_cfg.use_nerd_font
    {
        out.use_nerd_font = v;
        out.use_nerd_font_was_defaulted = false;
    }

    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn debug(msg: &str) {
    if std::env::var("CHEVRON_WEATHER_DEBUG").ok().as_deref() == Some("1") {
        eprintln!("chevron weather: {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WeatherConfig;

    #[test]
    fn merge_cli_overrides_toml() {
        let cli = Options {
            units: "imperial".into(),
            units_was_defaulted: false,
            ..Options::default()
        };
        let cfg = WeatherConfig {
            units: Some("metric".into()),
            ..WeatherConfig::default()
        };
        let merged = merge_options(&cli, &cfg);
        assert_eq!(merged.units, "imperial");
    }

    #[test]
    fn merge_toml_fills_in_cli_defaults() {
        let cli = Options::default(); // provider_was_defaulted = true
        let cfg = WeatherConfig {
            provider: Some("openweather".into()),
            api_key: Some("k".into()),
            units: Some("metric".into()),
            cache_ttl: Some(30),
            show_city: Some(false),
            show_icon: Some(false),
            use_nerd_font: Some(true),
            lat: Some(1.0),
            lon: Some(2.0),
            location_cmd: Some("cmd".into()),
        };
        let merged = merge_options(&cli, &cfg);
        assert_eq!(merged.provider, "openweather");
        assert_eq!(merged.units, "metric");
        assert_eq!(merged.cache_ttl_min, 30);
        assert!(!merged.show_city);
        assert!(!merged.show_icon);
        assert!(merged.use_nerd_font);
        assert!((merged.lat.unwrap() - 1.0).abs() < 1e-9);
        assert!((merged.lon.unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(merged.location_cmd.as_deref(), Some("cmd"));
        assert_eq!(merged.api_key.as_deref(), Some("k"));
    }
}
