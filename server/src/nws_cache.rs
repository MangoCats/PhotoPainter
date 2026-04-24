// Shared cache for the three NWS endpoint URLs that come from
// api.weather.gov/points/{lat},{lon}.  Both WeatherModule and RainModule
// need different URLs from that same response, so fetching it once avoids
// a duplicate request on every cold start.
use std::sync::Mutex;
use crate::location;

#[derive(Clone)]
pub struct NwsUrls {
    pub forecast:        String,
    pub forecast_hourly: String,
    pub forecast_grid:   String,
}

pub struct NwsPointsCache {
    cached: Mutex<Option<NwsUrls>>,
}

impl NwsPointsCache {
    pub fn new() -> Self {
        Self { cached: Mutex::new(None) }
    }

    pub async fn get(
        &self,
        client: &reqwest::Client,
    ) -> Result<NwsUrls, Box<dyn std::error::Error + Send + Sync>> {
        {
            if let Some(u) = self.cached.lock().unwrap().as_ref() {
                return Ok(u.clone());
            }
        }
        let url  = format!(
            "https://api.weather.gov/points/{:.4},{:.4}",
            location::LAT, location::LON
        );
        let body: serde_json::Value = client.get(&url).send().await?.json().await?;
        let props = &body["properties"];
        let urls  = NwsUrls {
            forecast:        props["forecast"].as_str().ok_or("missing forecast url")?.to_string(),
            forecast_hourly: props["forecastHourly"].as_str().ok_or("missing forecastHourly url")?.to_string(),
            forecast_grid:   props["forecastGridData"].as_str().ok_or("missing forecastGridData url")?.to_string(),
        };
        *self.cached.lock().unwrap() = Some(urls.clone());
        Ok(urls)
    }
}
