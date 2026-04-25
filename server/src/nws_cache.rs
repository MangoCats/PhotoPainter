// Shared cache for the three NWS endpoint URLs that come from
// api.weather.gov/points/{lat},{lon}.  Both WeatherModule and RainModule
// need different URLs from that same response, so fetching it once avoids
// a duplicate request on every cold start.
use std::sync::Mutex;
use std::time::{Duration, Instant};
use crate::location;

const TTL: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone)]
pub struct NwsUrls {
    pub forecast:             String,
    pub forecast_hourly:      String,
    pub forecast_grid:        String,
    pub observation_stations: String,
}

struct Cached {
    urls: NwsUrls,
    at:   Instant,
}

pub struct NwsPointsCache {
    inner: Mutex<Option<Cached>>,
}

impl NwsPointsCache {
    pub fn new() -> Self {
        Self { inner: Mutex::new(None) }
    }

    /// Drop the cached URLs so the next call re-fetches from NWS.
    /// Call this when a downstream request returns 404 (grid cell reassigned).
    #[allow(dead_code)]
    pub fn invalidate(&self) {
        *self.inner.lock().unwrap() = None;
    }

    pub async fn get(
        &self,
        client: &reqwest::Client,
    ) -> Result<NwsUrls, Box<dyn std::error::Error + Send + Sync>> {
        {
            if let Some(c) = self.inner.lock().unwrap().as_ref() {
                if c.at.elapsed() < TTL {
                    return Ok(c.urls.clone());
                }
                tracing::info!("NWS points cache expired, re-fetching");
            }
        }
        let url  = format!(
            "https://api.weather.gov/points/{:.4},{:.4}",
            location::LAT, location::LON
        );
        let body: serde_json::Value = client.get(&url).send().await?.json().await?;
        let props = &body["properties"];
        let urls  = NwsUrls {
            forecast:             props["forecast"].as_str().ok_or("missing forecast url")?.to_string(),
            forecast_hourly:      props["forecastHourly"].as_str().ok_or("missing forecastHourly url")?.to_string(),
            forecast_grid:        props["forecastGridData"].as_str().ok_or("missing forecastGridData url")?.to_string(),
            observation_stations: props["observationStations"].as_str().ok_or("missing observationStations url")?.to_string(),
        };
        *self.inner.lock().unwrap() = Some(Cached { urls: urls.clone(), at: Instant::now() });
        Ok(urls)
    }
}
