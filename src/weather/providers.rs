//! Provider trait + normalized weather data, plus the `resolve` factory.
//!
//! New providers implement [`Provider`] and are wired into [`resolve`].
//! Providers are responsible for mapping their native condition codes onto
//! the shared [`Condition`] enum so the formatter stays provider-agnostic.

use super::openmeteo::OpenMeteo;
use super::openweather::OpenWeather;

/// Normalized weather condition. Provider-specific codes map into this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Clear,
    PartlyCloudy,
    Cloudy,
    Rainy,
    Snow,
    Storm,
    Fog,
    Unknown,
}

/// Normalized weather payload returned by every [`Provider`].
///
/// `feels_like` and `description` are not consumed by the default format
/// string yet but are part of the public payload so that formatters and
/// provider authors can use them without an API break.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WeatherData {
    /// Best-effort city name. May be `None` when the provider doesn't include
    /// geocoding (open-meteo doesn't).
    pub city: Option<String>,
    /// ISO country code, best-effort.
    pub country: Option<String>,
    /// Temperature in the requested units (C for metric, F for imperial).
    pub temp: f64,
    /// "Feels like" temperature in the same units, if available.
    pub feels_like: Option<f64>,
    pub condition: Condition,
    /// Short human description like "light rain". May be empty.
    pub description: String,
}

/// A weather provider. Implementations MUST NOT panic and SHOULD honor the
/// 3-second timeout convention on their HTTP calls.
pub trait Provider {
    /// Fetch current conditions for `(lat, lon)` in the requested units.
    ///
    /// `units` is either `"metric"` or `"imperial"`.
    fn fetch(&self, lat: f64, lon: f64, units: &str) -> Result<WeatherData, String>;
}

/// Factory: build a provider by name. Caller passes the API key (only
/// `openweather` needs it).
pub fn resolve(name: &str, api_key: Option<&str>) -> Result<Box<dyn Provider>, String> {
    match name {
        "openmeteo" | "" => Ok(Box::new(OpenMeteo)),
        "openweather" => {
            let key = api_key.filter(|k| !k.is_empty()).ok_or_else(|| {
                String::from("openweather requires --api-key or CHEVRON_WEATHER_API_KEY")
            })?;
            Ok(Box::new(OpenWeather::new(key.to_string())))
        }
        other => Err(format!("unknown provider: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_openmeteo_by_default() {
        assert!(resolve("openmeteo", None).is_ok());
        assert!(resolve("", None).is_ok());
    }

    #[test]
    fn resolve_openweather_requires_key() {
        assert!(resolve("openweather", None).is_err());
        assert!(resolve("openweather", Some("")).is_err());
        assert!(resolve("openweather", Some("abc")).is_ok());
    }

    #[test]
    fn resolve_unknown_is_err() {
        assert!(resolve("darksky", None).is_err());
    }

    /// A mock [`Provider`] used from tests elsewhere to exercise the render
    /// pipeline without touching the network.
    pub struct MockProvider(pub WeatherData);

    impl Provider for MockProvider {
        fn fetch(&self, _lat: f64, _lon: f64, _units: &str) -> Result<WeatherData, String> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn mock_provider_returns_fixed_data() {
        let d = WeatherData {
            city: Some("Tacoma".into()),
            country: Some("US".into()),
            temp: 54.0,
            feels_like: Some(52.0),
            condition: Condition::Rainy,
            description: "light rain".into(),
        };
        let p = MockProvider(d.clone());
        let got = p.fetch(0.0, 0.0, "imperial").unwrap();
        assert!((got.temp - d.temp).abs() < f64::EPSILON);
        assert_eq!(got.condition, Condition::Rainy);
    }
}
