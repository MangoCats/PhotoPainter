use std::sync::Mutex;
use std::time::Duration;
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::location;
use super::{Module, Rect};

const CURRENT_SIZE_PX: f32 = 96.0;
const HL_SIZE_PX:      f32 = 43.0;
const MARGIN:          i32 = 4;
const HL_ROW_GAP:      i32 = 8;
const ICON_SIZE:       i32 = 64;
const ICON_GAP:        i32 = 8;

#[derive(Clone, Copy, Default, PartialEq)]
pub enum WeatherCondition {
    ClearDay, ClearNight,
    PartlyCloudyDay, PartlyCloudyNight,
    Cloudy, Rain, Thunderstorm, Snow, Fog,
    #[default] Unknown,
}

#[derive(Default, Clone, Copy)]
pub struct WeatherData {
    pub current_f: i32,
    pub high_f:    i32,
    pub low_f:     i32,
    pub condition: WeatherCondition,
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

    pub fn peek(&self) -> Option<WeatherData> {
        *self.data.lock().unwrap()
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
        let period0 = &hourly["properties"]["periods"][0];
        let current_f = period0["temperature"].as_i64().ok_or("missing current temp")? as i32;
        let condition = parse_condition(period0["icon"].as_str().unwrap_or(""));

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

        Ok(WeatherData { current_f, high_f, low_f, condition })
    }
}

fn parse_condition(icon_url: &str) -> WeatherCondition {
    let is_night = icon_url.contains("/night/");
    let path     = icon_url.split('?').next().unwrap_or(icon_url);
    let last_seg = path.rsplit('/').next().unwrap_or("");
    let raw_code = last_seg.split(',').next().unwrap_or("");
    let code     = raw_code.to_lowercase();

    if code.starts_with("tsra") || code.starts_with("thunderstorm") {
        return WeatherCondition::Thunderstorm;
    }
    match code.as_str() {
        "skc" | "few" | "wind_skc" | "wind_few" | "hot" | "cold" => {
            if is_night { WeatherCondition::ClearNight } else { WeatherCondition::ClearDay }
        }
        "sct" | "wind_sct" => {
            if is_night { WeatherCondition::PartlyCloudyNight } else { WeatherCondition::PartlyCloudyDay }
        }
        "bkn" | "ovc" | "wind_bkn" | "wind_ovc" => WeatherCondition::Cloudy,
        "rain" | "rain_showers" | "rain_showers_hi" | "fzra" | "sleet"
            | "rain_fzra" | "rain_sleet" => WeatherCondition::Rain,
        "snow" | "blizzard" | "snow_fzra" | "snow_sleet" => WeatherCondition::Snow,
        "fog" | "haze" => WeatherCondition::Fog,
        _ => WeatherCondition::Unknown,
    }
}

// ── Icon drawing ──────────────────────────────────────────────────────────────
// All coordinates are relative to the icon top-left (ix, iy).
// Icon box is ICON_SIZE × ICON_SIZE (64×64).

fn draw_cloud(canvas: &mut E6Canvas, ix: i32, iy: i32, color: E6Color) {
    // Three puffy bumps + filled base: spans x=[12,52], y=[12,38]
    canvas.fill_disc(ix + 32, iy + 22, 10, color); // center (tallest bump)
    canvas.fill_disc(ix + 20, iy + 27,  8, color); // left bump
    canvas.fill_disc(ix + 44, iy + 27,  8, color); // right bump
    canvas.fill_rect(ix + 12, iy + 27, 40, 11, color); // flat base
}

fn draw_small_cloud(canvas: &mut E6Canvas, ix: i32, iy: i32, color: E6Color) {
    // Smaller cloud in the lower-left for partly-cloudy icons
    canvas.fill_disc(ix + 22, iy + 44, 7, color);
    canvas.fill_disc(ix + 14, iy + 48, 5, color);
    canvas.fill_disc(ix + 30, iy + 48, 5, color);
    canvas.fill_rect(ix +  9, iy + 48, 26, 9, color);
}

fn draw_sun_full(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Large sun centered in the icon box
    canvas.fill_disc(ix + 32, iy + 30, 12, E6Color::Yellow);
    // Cardinal rays (3×6 px bars)
    canvas.fill_rect(ix + 31, iy + 12, 3, 6, E6Color::Yellow); // N
    canvas.fill_rect(ix + 31, iy + 44, 3, 6, E6Color::Yellow); // S
    canvas.fill_rect(ix + 46, iy + 29, 6, 3, E6Color::Yellow); // E
    canvas.fill_rect(ix + 12, iy + 29, 6, 3, E6Color::Yellow); // W
    // Diagonal rays (4×4 px squares)
    canvas.fill_rect(ix + 43, iy + 15, 4, 4, E6Color::Yellow); // NE
    canvas.fill_rect(ix + 43, iy + 41, 4, 4, E6Color::Yellow); // SE
    canvas.fill_rect(ix + 17, iy + 41, 4, 4, E6Color::Yellow); // SW
    canvas.fill_rect(ix + 17, iy + 15, 4, 4, E6Color::Yellow); // NW
}

fn draw_sun_small(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Small sun in upper-right, for partly-cloudy-day icon
    canvas.fill_disc(ix + 45, iy + 18, 9, E6Color::Yellow);
    canvas.fill_rect(ix + 44, iy +  5, 3, 5, E6Color::Yellow); // N
    canvas.fill_rect(ix + 44, iy + 29, 3, 5, E6Color::Yellow); // S
    canvas.fill_rect(ix + 56, iy + 17, 5, 3, E6Color::Yellow); // E
    canvas.fill_rect(ix + 29, iy + 17, 5, 3, E6Color::Yellow); // W
    canvas.fill_rect(ix + 53, iy +  9, 3, 3, E6Color::Yellow); // NE
    canvas.fill_rect(ix + 53, iy + 26, 3, 3, E6Color::Yellow); // SE
    canvas.fill_rect(ix + 35, iy + 26, 3, 3, E6Color::Yellow); // SW
    canvas.fill_rect(ix + 35, iy +  9, 3, 3, E6Color::Yellow); // NW
}

fn draw_moon_full(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Crescent moon centered: yellow disc with white disc offset to carve crescent
    canvas.fill_disc(ix + 29, iy + 30, 15, E6Color::Yellow);
    canvas.fill_disc(ix + 37, iy + 24, 12, E6Color::White);
}

fn draw_moon_small(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Small crescent moon in upper-right, for partly-cloudy-night icon
    canvas.fill_disc(ix + 44, iy + 19, 10, E6Color::Yellow);
    canvas.fill_disc(ix + 50, iy + 14,  8, E6Color::White);
}

fn draw_weather_icon(canvas: &mut E6Canvas, ix: i32, iy: i32, cond: WeatherCondition) {
    match cond {
        WeatherCondition::ClearDay => {
            draw_sun_full(canvas, ix, iy);
        }
        WeatherCondition::ClearNight => {
            draw_moon_full(canvas, ix, iy);
        }
        WeatherCondition::PartlyCloudyDay => {
            draw_sun_small(canvas, ix, iy);
            draw_small_cloud(canvas, ix, iy, E6Color::Blue);
        }
        WeatherCondition::PartlyCloudyNight => {
            draw_moon_small(canvas, ix, iy);
            draw_small_cloud(canvas, ix, iy, E6Color::Blue);
        }
        WeatherCondition::Cloudy => {
            draw_cloud(canvas, ix, iy, E6Color::Blue);
        }
        WeatherCondition::Rain => {
            draw_cloud(canvas, ix, iy, E6Color::Blue);
            canvas.fill_rect(ix + 20, iy + 42, 3, 9, E6Color::Blue);
            canvas.fill_rect(ix + 30, iy + 42, 3, 9, E6Color::Blue);
            canvas.fill_rect(ix + 40, iy + 42, 3, 9, E6Color::Blue);
        }
        WeatherCondition::Thunderstorm => {
            draw_cloud(canvas, ix, iy, E6Color::Blue);
            // Z-shaped lightning bolt
            canvas.fill_rect(ix + 28, iy + 40, 10, 3, E6Color::Yellow);
            canvas.fill_rect(ix + 28, iy + 43,  4, 9, E6Color::Yellow);
            canvas.fill_rect(ix + 22, iy + 50, 10, 3, E6Color::Yellow);
            canvas.fill_rect(ix + 22, iy + 53,  4, 8, E6Color::Yellow);
        }
        WeatherCondition::Snow => {
            draw_cloud(canvas, ix, iy, E6Color::Blue);
            // Three cross-shaped snowflakes
            for &cx in &[18i32, 30, 42] {
                canvas.fill_rect(ix + cx - 4, iy + 47, 9, 3, E6Color::Blue);
                canvas.fill_rect(ix + cx - 1, iy + 43, 3, 9, E6Color::Blue);
            }
        }
        WeatherCondition::Fog => {
            canvas.fill_rect(ix +  6, iy + 18, 52, 4, E6Color::Black);
            canvas.fill_rect(ix + 10, iy + 26, 44, 4, E6Color::Black);
            canvas.fill_rect(ix +  6, iy + 34, 52, 4, E6Color::Black);
            canvas.fill_rect(ix + 10, iy + 42, 44, 4, E6Color::Black);
            canvas.fill_rect(ix +  6, iy + 50, 52, 4, E6Color::Black);
        }
        WeatherCondition::Unknown => {}
    }
}

// ── Module impl ───────────────────────────────────────────────────────────────

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

        let half_char = measure_text("0", HL_SIZE_PX, false).0 / 2;

        let hl_right    = region.x + region.width - MARGIN;
        let hl_col_w    = high_w.max(low_w);
        let hl_x_high   = hl_right - high_w;
        let hl_x_low    = hl_right - low_w;
        let hl_col_left = hl_right - hl_col_w;

        let cur_x = hl_col_left - half_char - cur_w;

        let hl_total = hl_a * 2 + HL_ROW_GAP;
        let block_h  = cur_a.max(hl_total);
        let top_y    = region.y + MARGIN;
        let cur_y    = top_y + (block_h - cur_a)    / 2;
        let hl_y     = top_y + (block_h - hl_total) / 2;

        let icon_x = cur_x - ICON_GAP - ICON_SIZE;
        let icon_y = top_y + (block_h - ICON_SIZE) / 2;

        // Erase from icon left edge (or region left) to right margin
        let erase_x = icon_x.max(region.x);
        let erase_w = (region.x + region.width) - erase_x;
        canvas.fill_rect(erase_x, top_y, erase_w, block_h, E6Color::White);

        draw_weather_icon(canvas, icon_x, icon_y, d.condition);

        draw_text(canvas, cur_x,     cur_y,                    &cur_str,  CURRENT_SIZE_PX, E6Color::Green, true);
        draw_text(canvas, hl_x_high, hl_y,                    &high_str, HL_SIZE_PX,      E6Color::Green, false);
        draw_text(canvas, hl_x_low,  hl_y + hl_a + HL_ROW_GAP, &low_str, HL_SIZE_PX,    E6Color::Green, false);
    }

    fn data_refresh_interval(&self) -> Duration { Duration::from_secs(300) }
    fn suggested_poll_interval(&self) -> Option<Duration> { None }
}
