//! Reverse geocoding (GPS → "City, Country") via OpenStreetMap Nominatim.
//!
//! Nominatim's usage policy is respected: an identifying User-Agent, at most
//! one request per second, and results are cached (in memory here and in the
//! catalog by the caller) by coordinates rounded to ~100 m, so a batch of
//! photos from the same place costs one request.
//!
//! Privacy: this sends coordinates to openstreetmap.org. Only call it after
//! the user opted in.

use crate::error::{Error, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://nominatim.openstreetmap.org/reverse";
const USER_AGENT: &str =
    concat!("PrismStudio/", env!("CARGO_PKG_VERSION"), " (+https://github.com/nitzan123/nitzan123)");
const MIN_INTERVAL: Duration = Duration::from_millis(1100);
/// After a network failure, skip lookups for this long so a large export
/// doesn't wait out a timeout per photo while offline.
const OFFLINE_BACKOFF: Duration = Duration::from_secs(120);

pub struct Geocoder {
    agent: ureq::Agent,
    last_request: Mutex<Option<Instant>>,
    memo: Mutex<HashMap<String, Option<String>>>,
    offline_since: Mutex<Option<Instant>>,
}

impl Default for Geocoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Geocoder {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(15))
            .try_proxy_from_env(true)
            .build();
        Self {
            agent,
            last_request: Mutex::new(None),
            memo: Mutex::new(HashMap::new()),
            offline_since: Mutex::new(None),
        }
    }

    /// Place name for the coordinates, `None` if OSM knows no place there
    /// (e.g. open sea). Network errors are returned as `Err`. `language` is
    /// an Accept-Language value such as `en` or `he`; empty = local names.
    pub fn reverse(&self, lat: f64, lon: f64, language: &str) -> Result<Option<String>> {
        self.lookup(lat, lon, language, PlaceDetail::CityCountry)
    }

    /// Like [`Geocoder::reverse`] with a choice of detail.
    pub fn lookup(&self, lat: f64, lon: f64, language: &str, detail: PlaceDetail) -> Result<Option<String>> {
        let key = format!("{}|{language}|{detail:?}", cache_key(lat, lon));
        if let Some(hit) = self.memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(hit.clone());
        }
        if let Some(t) = *self.offline_since.lock().unwrap_or_else(|e| e.into_inner()) {
            if t.elapsed() < OFFLINE_BACKOFF {
                return Err(Error::Network("offline (recent lookup failed)".into()));
            }
        }
        self.throttle();
        let mut req = self
            .agent
            .get(ENDPOINT)
            .query("format", "jsonv2")
            .query("lat", &format!("{lat:.5}"))
            .query("lon", &format!("{lon:.5}"))
            .query("zoom", "10"); // city level
        if !language.is_empty() {
            req = req.query("accept-language", language);
        }
        let resp = match req.call() {
            Ok(r) => r,
            // A transport failure means we're offline/blocked: back off.
            // HTTP errors (e.g. 429) also back off; Nominatim asks for that.
            Err(e) => {
                *self.offline_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                return Err(Error::Network(e.to_string()));
            }
        };
        *self.offline_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let json: Value = serde_json::from_reader(resp.into_reader())?;
        let place = match detail {
            PlaceDetail::CityCountry => format_place(&json),
            PlaceDetail::City => format_locality(&json),
        };
        self.memo.lock().unwrap_or_else(|e| e.into_inner()).insert(key, place.clone());
        Ok(place)
    }

    fn throttle(&self) {
        let mut last = self.last_request.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < MIN_INTERVAL {
                std::thread::sleep(MIN_INTERVAL - since);
            }
        }
        *last = Some(Instant::now());
    }
}

/// Coordinates rounded to 3 decimals (~110 m) — the cache granularity.
pub fn cache_key(lat: f64, lon: f64) -> String {
    format!("{lat:.3},{lon:.3}")
}

/// "City, Country" from a Nominatim `jsonv2` response.
pub fn format_place(json: &Value) -> Option<String> {
    if json.get("error").is_some() {
        return None;
    }
    let addr = json.get("address");
    let field = |k: &str| addr.and_then(|a| a.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty());
    let locality = ["city", "town", "village", "municipality", "hamlet", "suburb", "county", "state"]
        .iter()
        .find_map(|k| field(k));
    let country = field("country");
    match (locality, country) {
        (Some(l), Some(c)) if l != c => Some(format!("{l}, {c}")),
        (Some(l), _) => Some(l.to_string()),
        (None, Some(c)) => Some(c.to_string()),
        (None, None) => json
            .get("display_name")
            .and_then(Value::as_str)
            .map(|d| d.split(", ").take(2).collect::<Vec<_>>().join(", "))
            .filter(|s| !s.is_empty()),
    }
}

/// How much of the place name to keep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaceDetail {
    /// `Chinchero`
    #[default]
    City,
    /// `Chinchero, Perú`
    CityCountry,
}

/// Just the town or city, without administrative prefixes such as
/// "Distrito de" (Peru) — e.g. `Cusco`, `Chinchero`, `Tel Aviv-Yafo`.
pub fn format_locality(json: &Value) -> Option<String> {
    if json.get("error").is_some() {
        return None;
    }
    let addr = json.get("address")?;
    let field = |k: &str| addr.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let name = ["city", "town", "village", "municipality", "hamlet", "suburb", "county", "state"]
        .iter()
        .find_map(|k| field(k))?;
    const PREFIXES: [&str; 8] = [
        "Distrito de ",
        "Provincia de ",
        "Municipio de ",
        "Municipalidad de ",
        "Comune di ",
        "Gemeinde ",
        "Commune de ",
        "District of ",
    ];
    let trimmed = PREFIXES.iter().find_map(|p| name.strip_prefix(p)).unwrap_or(name);
    Some(trimmed.trim().to_string())
}

/// Human-readable coordinates, e.g. `32.0853° N, 34.7818° E`.
pub fn format_coords(lat: f64, lon: f64) -> String {
    format!(
        "{:.4}° {}, {:.4}° {}",
        lat.abs(),
        if lat >= 0.0 { 'N' } else { 'S' },
        lon.abs(),
        if lon >= 0.0 { 'E' } else { 'W' }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn locality_drops_country_and_admin_prefixes() {
        let j = |a: serde_json::Value| serde_json::json!({ "address": a });
        assert_eq!(
            format_locality(&j(serde_json::json!({"city": "Distrito de Cusco", "state": "Cusco"}))).as_deref(),
            Some("Cusco")
        );
        assert_eq!(
            format_locality(&j(serde_json::json!({"town": "Chinchero", "country": "Perú"}))).as_deref(),
            Some("Chinchero")
        );
        assert_eq!(format_locality(&serde_json::json!({"error": "Unable to geocode"})), None);
    }

    #[test]
    fn formats_city_and_country() {
        let v = json!({"address": {"city": "Tel-Aviv", "state": "Tel-Aviv District", "country": "Israel"}});
        assert_eq!(format_place(&v).as_deref(), Some("Tel-Aviv, Israel"));
        let v = json!({"address": {"village": "Ein Gedi", "country": "Israel"}});
        assert_eq!(format_place(&v).as_deref(), Some("Ein Gedi, Israel"));
        let v = json!({"address": {"country": "Iceland"}});
        assert_eq!(format_place(&v).as_deref(), Some("Iceland"));
        assert_eq!(format_place(&json!({"error": "Unable to geocode"})), None);
    }

    #[test]
    fn coords_and_cache_key() {
        assert_eq!(format_coords(32.08531, -34.78181), "32.0853° N, 34.7818° W");
        assert_eq!(cache_key(32.08531, 34.78181), cache_key(32.0849, 34.7822));
    }
}
