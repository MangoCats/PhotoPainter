use std::sync::Mutex;
use std::time::Duration;
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::location;
use super::{Module, Rect};

const CURRENT_SIZE_PX: f32 = 96.0;  // 20% of 480
const HL_SIZE_PX:      f32 = 43.0;  // ~9% of 480
const MARGIN:          i32 = 16;
const HL_ROW_GAP:      i32 = 8;

#[derive(Default, Clone, Copy)]
struct WeatherData {
    current_f: i32,
    high_f:    i32,
    low_f:     i32,
}

struct CachedUrls {
    forecast:        String,
    forecast_hourly: String,
}

pub struct WeatherModule {
    data:   Mutex<Option<WeatherData>>,
    urls:   Mutex<Option<CachedUrls>>,
    client: reqwest::Client,
}

impl WeatherModule {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("PhotoPainter/1.0 (github.com/photopainter)")
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");
        Self { data: Mutex::new(None), urls: Mutex::new(None), client }
    }

    pub async fn refresh(&self) {
        match self.fetch().await {
            Ok(d)  => *self.data.lock().unwrap() = Some(d),
            Err(e) => tracing::warn!("weather fetch failed: {e}"),
        }
    }

    async fn forecast_urls(&self) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        {
            let g = self.urls.lock().unwrap();
            if let Some(u) = g.as_ref() {
                return Ok((u.forecast.clone(), u.forecast_hourly.clone()));
            }
        }
        let url  = format!("https://api.weather.gov/points/{:.4},{:.4}", location::LAT, location::LON);
        let body: serde_json::Value = self.client.get(&url).send().await?.json().await?;
        let props = &body["properties"];
        let forecast = props["forecast"].as_str().ok_or("missing forecast url")?.to_string();
        let hourly   = props["forecastHourly"].as_str().ok_or("missing forecastHourly url")?.to_string();
        *self.urls.lock().unwrap() = Some(CachedUrls { forecast: forecast.clone(), forecast_hourly: hourly.clone() });
        Ok((forecast, hourly))
    }

    async fn fetch(&self) -> Result<WeatherData, Box<dyn std::error::Error + Send + Sync>> {
        let (forecast_url, hourly_url) = self.forecast_urls().await?;

        let hourly: serde_json::Value = self.client.get(&hourly_url).send().await?.json().await?;
        let current_f = hourly["properties"]["periods"][0]["temperature"]
            .as_i64().ok_or("missing current temp")? as i32;

        let daily: serde_json::Value = self.client.get(&forecast_url).send().await?.json().await?;
        let periods = daily["properties"]["periods"].as_array().ok_or("missing periods")?;

        let high_f = periods.iter()
            .find(|p| p["isDaytime"].as_bool().unwrap_or(false))
            .and_then(|p| p["temperature"].as_i64())
            .ok_or("missing high temp")? as i32;

        let low_f = periods.iter()
            .find(|p| !p["isDaytime"].as_bool().unwrap_or(true))
            .and_then(|p| p["temperature"].as_i64())
            .ok_or("missing low temp")? as i32;

        Ok(WeatherData { current_f, high_f, low_f })
    }
}

impl Module for WeatherModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let guard = self.data.lock().unwrap();
        let Some(d) = *guard else { return };
        drop(guard);

        let cur_str  = format!("{}°", d.current_f);
        let high_str = format!("{}", d.high_f);
        let low_str  = format!("{}", d.low_f);

        let (cur_w,  cur_a) = measure_text(&cur_str,  CURRENT_SIZE_PX, true);
        let (high_w, hl_a)  = measure_text(&high_str, HL_SIZE_PX,      false);
        let (low_w,  _)     = measure_text(&low_str,  HL_SIZE_PX,      false);

        // Half a character-width at the H/L font size
        let half_char = measure_text("0", HL_SIZE_PX, false).0 / 2;

        // Right-align H/L numbers to the right margin; right-align each individually
        let hl_right  = region.x + region.width - MARGIN;
        let hl_col_w  = high_w.max(low_w);          // widest number sets the column
        let hl_x_high = hl_right - high_w;           // right-align each number
        let hl_x_low  = hl_right - low_w;
        let hl_col_left = hl_right - hl_col_w;       // left edge of the column

        // Current temp: ° right edge sits half_char to the left of the H/L column
        let cur_x = hl_col_left - half_char - cur_w;

        // Vertical: current temp and H/L pair share the same top anchor, each
        // centred against the taller of the two, starting at the top margin
        let hl_total = hl_a * 2 + HL_ROW_GAP;
        let block_h  = cur_a.max(hl_total);
        let top_y    = region.y + MARGIN;
        let cur_y    = top_y + (block_h - cur_a)    / 2;
        let hl_y     = top_y + (block_h - hl_total) / 2;

        draw_text(canvas, cur_x,    cur_y,                  &cur_str,  CURRENT_SIZE_PX, E6Color::Green, true);
        draw_text(canvas, hl_x_high, hl_y,                  &high_str, HL_SIZE_PX,      E6Color::Green, false);
        draw_text(canvas, hl_x_low,  hl_y + hl_a + HL_ROW_GAP, &low_str, HL_SIZE_PX,  E6Color::Green, false);
    }

    fn data_refresh_interval(&self) -> Duration { Duration::from_secs(300) }
    fn suggested_poll_interval(&self) -> Option<Duration> { None }
}
