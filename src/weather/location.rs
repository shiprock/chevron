//! Location resolver.
//!
//! Priority (highest first):
//!
//! 1. `opts.lat` + `opts.lon` (from `--lat` / `--lon` or `CHEVRON_WEATHER_LAT` /
//!    `CHEVRON_WEATHER_LON`).
//! 2. `opts.location_cmd` — run under `sh -c`; stdout must be `"lat|lon"`.
//! 3. IP geolocation via ifconfig.co JSON (zero-config default).
//!
//! IP geolocation also returns a best-effort city + country that we stash in
//! [`Location`] so the formatter can show `"Tacoma, US"` for Open-Meteo
//! (which doesn't include geocoding in its response).

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::Options;

const HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const LOCATION_CMD_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub city: Option<String>,
    pub country: Option<String>,
}

pub fn resolve(opts: &Options) -> Result<Location, String> {
    if let (Some(lat), Some(lon)) = (opts.lat, opts.lon) {
        return Ok(Location {
            lat,
            lon,
            city: None,
            country: None,
        });
    }

    if let Some(cmd) = opts.location_cmd.as_deref()
        && let Some((lat, lon)) = run_location_cmd(cmd)
    {
        return Ok(Location {
            lat,
            lon,
            city: None,
            country: None,
        });
    }

    ip_geolocate()
}

/// Parse `"lat|lon"` (the contract for `--location-cmd` stdout).
fn parse_lat_lon(s: &str) -> Option<(f64, f64)> {
    let (l, r) = s.trim().split_once('|')?;
    let lat = l.trim().parse::<f64>().ok()?;
    let lon = r.trim().parse::<f64>().ok()?;
    if lat.is_finite() && lon.is_finite() {
        Some((lat, lon))
    } else {
        None
    }
}

fn run_location_cmd(cmd: &str) -> Option<(f64, f64)> {
    let mut child = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = child.stdout.take()?;
                let mut buf = String::new();
                stdout.read_to_string(&mut buf).ok()?;
                return parse_lat_lon(&buf);
            }
            Ok(None) => {
                if start.elapsed() > LOCATION_CMD_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

fn ip_geolocate() -> Result<Location, String> {
    // ifconfig.co returns JSON when the Accept header asks for it.
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .new_agent();

    let body: serde_json::Value = agent
        .get("https://ifconfig.co/json")
        .header("accept", "application/json")
        .call()
        .map_err(|e| format!("ifconfig.co http: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("ifconfig.co json: {e}"))?;

    let lat = body
        .get("latitude")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| String::from("ifconfig.co: missing latitude"))?;
    let lon = body
        .get("longitude")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| String::from("ifconfig.co: missing longitude"))?;

    let city = body
        .get("city")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let country = body
        .get("country_iso")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    Ok(Location {
        lat,
        lon,
        city,
        country,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn clear_env() {
        for k in [
            "CHEVRON_WEATHER_LAT",
            "CHEVRON_WEATHER_LON",
            "CHEVRON_WEATHER_LOCATION_CMD",
        ] {
            // SAFETY: test-only
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    fn parses_lat_pipe_lon() {
        assert_eq!(parse_lat_lon("47.13|-122.16"), Some((47.13, -122.16)));
        assert_eq!(parse_lat_lon("  47.13 | -122.16  "), Some((47.13, -122.16)));
    }

    #[test]
    fn rejects_bad_format() {
        assert!(parse_lat_lon("47.13").is_none());
        assert!(parse_lat_lon("47.13,-122.16").is_none());
        assert!(parse_lat_lon("a|b").is_none());
    }

    #[test]
    #[serial]
    fn lat_lon_flags_take_priority() {
        clear_env();
        let opts = Options {
            lat: Some(1.0),
            lon: Some(2.0),
            location_cmd: Some("echo 99|99".into()),
            ..Options::default()
        };
        let loc = resolve(&opts).unwrap();
        assert!((loc.lat - 1.0).abs() < 1e-9);
        assert!((loc.lon - 2.0).abs() < 1e-9);
    }

    #[test]
    #[serial]
    fn location_cmd_used_when_no_lat_lon() {
        clear_env();
        let opts = Options {
            location_cmd: Some("printf '47.5|-122.5'".into()),
            ..Options::default()
        };
        let loc = resolve(&opts).unwrap();
        assert!((loc.lat - 47.5).abs() < 1e-9);
        assert!((loc.lon - -122.5).abs() < 1e-9);
    }

    #[test]
    #[serial]
    fn location_cmd_bad_output_falls_back_to_ip_or_err() {
        clear_env();
        let opts = Options {
            location_cmd: Some("echo not-lat-lon".into()),
            ..Options::default()
        };
        // Expected behavior: cmd fails to parse, falls through to ip_geolocate.
        // We can't reliably assert on network here; just require no panic.
        let _ = resolve(&opts);
    }
}
