use std::sync::{Arc, Mutex};
use chrono::{DateTime, FixedOffset, Utc};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::nws_cache::NwsPointsCache;
use super::{Module, Rect};
use super::battery::BatteryInfo;

const CURRENT_SIZE_PX: f32 = 96.0;
const HL_SIZE_PX:      f32 = 43.0;
const MARGIN:          i32 = 4;
const HL_ROW_GAP:      i32 = 4;
pub(crate) const ICON_SIZE: i32 = 64;
const ICON_GAP:        i32 = 8;
const ICON_BG_R:       i32 = 10;  // rounded-corner radius for icon background

const BATT_FONT_PX:   f32 = 14.0;
const BATT_TOP_PAD:   i32 = 2;
const BATT_GAP:       i32 = 3;
const BATT_ICON_BODY: i32 = 19;
const BATT_ICON_NUB:  i32 = 3;
const BATT_ICON_W:    i32 = 22;  // BODY + NUB
const BATT_ICON_H:    i32 = 10;
const BATT_ICON_SEP:  i32 = 4;   // gap between icon and text

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

pub struct WeatherModule {
    data:        Mutex<Option<WeatherData>>,
    battery:     Mutex<Option<BatteryInfo>>,
    obs_station: Mutex<Option<String>>,   // cached URL for the nearest obs station
    nws_cache:   Arc<NwsPointsCache>,
    client:      reqwest::Client,
}

impl WeatherModule {
    pub fn new(client: reqwest::Client, nws_cache: Arc<NwsPointsCache>) -> Self {
        Self {
            data:        Mutex::new(None),
            battery:     Mutex::new(None),
            obs_station: Mutex::new(None),
            nws_cache,
            client,
        }
    }

    pub fn peek(&self) -> Option<WeatherData> {
        *self.data.lock().unwrap()
    }

    pub fn peek_battery(&self) -> Option<BatteryInfo> {
        self.battery.lock().unwrap().clone()
    }

    pub fn update_battery(&self, info: Option<BatteryInfo>) {
        *self.battery.lock().unwrap() = info;
    }

    pub async fn refresh(&self) {
        match self.fetch().await {
            Ok(d)  => *self.data.lock().unwrap() = Some(d),
            Err(e) => tracing::warn!("weather fetch failed: {e}"),
        }
    }

    async fn fetch(&self) -> Result<WeatherData, Box<dyn std::error::Error + Send + Sync>> {
        let urls = self.nws_cache.get(&self.client).await?;

        // Hourly forecast: used for condition icon and as fallback current temp
        let hourly: serde_json::Value = self.client.get(&urls.forecast_hourly).send().await?.json().await?;
        let period0   = &hourly["properties"]["periods"][0];
        let hourly_f  = period0["temperature"].as_i64().ok_or("missing current temp")? as i32;
        let condition = parse_condition(period0["icon"].as_str().unwrap_or(""));

        // Real-time station observation for current temp; falls back to hourly forecast
        let current_f = self.current_temp_from_obs(&urls.observation_stations).await
            .unwrap_or(hourly_f);

        let daily: serde_json::Value = self.client.get(&urls.forecast).send().await?.json().await?;
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

    /// Returns the `/observations/latest` URL for the nearest station,
    /// fetching and caching the station list on first call.
    async fn obs_station_url(
        &self,
        stations_list_url: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        {
            if let Some(url) = self.obs_station.lock().unwrap().as_ref() {
                return Ok(url.clone());
            }
        }
        let body: serde_json::Value = self.client.get(stations_list_url).send().await?.json().await?;
        let station_id = body["features"][0]["properties"]["stationIdentifier"]
            .as_str()
            .ok_or("no observation station in list")?;
        let url = format!("https://api.weather.gov/stations/{station_id}/observations/latest");
        *self.obs_station.lock().unwrap() = Some(url.clone());
        Ok(url)
    }

    /// Fetches the latest station observation and returns the temperature in °F,
    /// or `None` if the observation is unavailable, stale (>2 h), or fails QC.
    async fn current_temp_from_obs(&self, stations_list_url: &str) -> Option<i32> {
        let obs_url = match self.obs_station_url(stations_list_url).await {
            Ok(u)  => u,
            Err(e) => { tracing::warn!("obs station lookup failed: {e}"); return None; }
        };

        let resp = match self.client.get(&obs_url).send().await {
            Ok(r)  => r,
            Err(e) => { tracing::warn!("obs fetch failed: {e}"); return None; }
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(b)  => b,
            Err(e) => { tracing::warn!("obs parse failed: {e}"); return None; }
        };

        let props = &body["properties"];

        // Reject stale observations (station may be offline or slow to report)
        if let Some(ts_str) = props["timestamp"].as_str() {
            if let Ok(ts) = ts_str.parse::<DateTime<FixedOffset>>() {
                let age_min = Utc::now()
                    .signed_duration_since(ts.with_timezone(&Utc))
                    .num_minutes();
                if age_min > 120 {
                    tracing::warn!("observation stale ({age_min} min old), using forecast");
                    return None;
                }
            }
        }

        // Accept V (verified), Z (preliminary), G (auto QC passed)
        let qc = props["temperature"]["qualityControl"].as_str().unwrap_or("X");
        if !matches!(qc, "V" | "Z" | "G") {
            tracing::warn!("observation QC rejected: {qc}");
            return None;
        }

        let celsius = props["temperature"]["value"].as_f64()?;
        Some(((celsius * 9.0 / 5.0) + 32.0).round() as i32)
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

/// Returns the inclusive x range [x0, x1) for row `dy` of a rounded rectangle
/// of `size × size` with corner radius `r`.
fn icon_bg_x_range(dy: i32, size: i32, r: i32) -> (i32, i32) {
    let diff = if dy < r {
        (r - dy) as f64
    } else if dy >= size - r {
        (dy - (size - 1 - r)) as f64
    } else {
        return (0, size);
    };
    let dx = ((r * r) as f64 - diff * diff).max(0.0).sqrt() as i32;
    (r - dx, size - r + dx)
}

/// Draws a rounded-rectangle background for the icon area.
/// Daytime: white base + 25% blue dither.  Nighttime: black base + 25% blue dither.
fn draw_icon_bg(canvas: &mut E6Canvas, ix: i32, iy: i32, is_night: bool) {
    let bg = if is_night { E6Color::Black } else { E6Color::White };
    for dy in 0..ICON_SIZE {
        let (x0, x1) = icon_bg_x_range(dy, ICON_SIZE, ICON_BG_R);
        if x0 >= x1 { continue; }
        canvas.fill_rect(ix + x0, iy + dy, x1 - x0, 1, bg);
        // 25% blue dither: every even row, every even column
        if dy % 2 == 0 {
            let mut x = if x0 % 2 == 0 { x0 } else { x0 + 1 };
            while x < x1 {
                canvas.fill_rect(ix + x, iy + dy, 1, 1, E6Color::Blue);
                x += 2;
            }
        }
    }
}

fn draw_cloud(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Two-bump cloud: large left, small right, wide body — blue outline then white fill
    canvas.fill_disc(ix + 21, iy + 22, 13, E6Color::Blue);  // left big bump outline
    canvas.fill_disc(ix + 41, iy + 28, 10, E6Color::Blue);  // right small bump outline
    canvas.fill_rect(ix +  7, iy + 26, 50, 14, E6Color::Blue); // body outline
    canvas.fill_disc(ix + 21, iy + 22, 12, E6Color::White);
    canvas.fill_disc(ix + 41, iy + 28,  9, E6Color::White);
    canvas.fill_rect(ix +  8, iy + 27, 48, 13, E6Color::White);
}

fn draw_small_cloud(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    // Radii sum (7+5=12) ≈ center separation (~11px) so bumps just touch, matching main cloud proportions
    canvas.fill_disc(ix + 16, iy + 44, 8, E6Color::Blue);   // left big bump outline
    canvas.fill_disc(ix + 27, iy + 47, 6, E6Color::Blue);   // right small bump outline
    canvas.fill_rect(ix +  8, iy + 46, 26, 12, E6Color::Blue);
    canvas.fill_disc(ix + 16, iy + 44, 7, E6Color::White);
    canvas.fill_disc(ix + 27, iy + 47, 5, E6Color::White);
    canvas.fill_rect(ix +  9, iy + 47, 24, 10, E6Color::White);
}

fn draw_sun_full(canvas: &mut E6Canvas, ix: i32, iy: i32) {
    canvas.fill_disc(ix + 32, iy + 30, 12, E6Color::Yellow);
    canvas.fill_rect(ix + 31, iy + 12, 3, 6, E6Color::Yellow); // N
    canvas.fill_rect(ix + 31, iy + 44, 3, 6, E6Color::Yellow); // S
    canvas.fill_rect(ix + 46, iy + 29, 6, 3, E6Color::Yellow); // E
    canvas.fill_rect(ix + 12, iy + 29, 6, 3, E6Color::Yellow); // W
    canvas.fill_rect(ix + 43, iy + 15, 4, 4, E6Color::Yellow); // NE
    canvas.fill_rect(ix + 43, iy + 41, 4, 4, E6Color::Yellow); // SE
    canvas.fill_rect(ix + 17, iy + 41, 4, 4, E6Color::Yellow); // SW
    canvas.fill_rect(ix + 17, iy + 15, 4, 4, E6Color::Yellow); // NW
}

fn draw_sun_small(canvas: &mut E6Canvas, ix: i32, iy: i32) {
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

fn draw_moon_full(canvas: &mut E6Canvas, ix: i32, iy: i32, cutout: E6Color) {
    canvas.fill_disc(ix + 29, iy + 30, 15, E6Color::Yellow);
    canvas.fill_disc(ix + 37, iy + 24, 12, cutout);  // carve crescent

    // Four single-pixel stars scattered outside the moon
    for &(sx, sy) in &[(7i32, 9i32), (54, 6), (58, 35), (12, 55)] {
        canvas.fill_rect(ix + sx, iy + sy, 1, 1, E6Color::White);
    }
    // One bright cross star: yellow center, four white arms
    canvas.fill_rect(ix + 48, iy + 54,     1, 1, E6Color::Yellow);
    canvas.fill_rect(ix + 47, iy + 54,     1, 1, E6Color::White);
    canvas.fill_rect(ix + 49, iy + 54,     1, 1, E6Color::White);
    canvas.fill_rect(ix + 48, iy + 53,     1, 1, E6Color::White);
    canvas.fill_rect(ix + 48, iy + 55,     1, 1, E6Color::White);
}

fn draw_moon_small(canvas: &mut E6Canvas, ix: i32, iy: i32, cutout: E6Color) {
    canvas.fill_disc(ix + 44, iy + 19, 10, E6Color::Yellow);
    canvas.fill_disc(ix + 50, iy + 14,  8, cutout);  // carve crescent
}

pub(crate) fn draw_condition_icon(canvas: &mut E6Canvas, ix: i32, iy: i32, cond: WeatherCondition) {
    draw_weather_icon(canvas, ix, iy, cond);
}

fn draw_weather_icon(canvas: &mut E6Canvas, ix: i32, iy: i32, cond: WeatherCondition) {
    let is_night    = matches!(cond, WeatherCondition::ClearNight | WeatherCondition::PartlyCloudyNight);
    let moon_cutout = if is_night { E6Color::Black } else { E6Color::White };

    draw_icon_bg(canvas, ix, iy, is_night);

    match cond {
        WeatherCondition::ClearDay          => draw_sun_full(canvas, ix, iy),
        WeatherCondition::ClearNight        => draw_moon_full(canvas, ix, iy, moon_cutout),
        WeatherCondition::PartlyCloudyDay   => {
            draw_sun_small(canvas, ix, iy);
            draw_small_cloud(canvas, ix, iy);
        }
        WeatherCondition::PartlyCloudyNight => {
            draw_moon_small(canvas, ix, iy, moon_cutout);
            draw_small_cloud(canvas, ix, iy);
        }
        WeatherCondition::Cloudy            => draw_cloud(canvas, ix, iy),
        WeatherCondition::Rain              => {
            draw_cloud(canvas, ix, iy);
            canvas.fill_rect(ix + 20, iy + 42, 3, 9, E6Color::Blue);
            canvas.fill_rect(ix + 30, iy + 42, 3, 9, E6Color::Blue);
            canvas.fill_rect(ix + 40, iy + 42, 3, 9, E6Color::Blue);
        }
        WeatherCondition::Thunderstorm      => {
            draw_cloud(canvas, ix, iy);
            // Z-shaped lightning bolt
            canvas.fill_rect(ix + 28, iy + 40, 10, 3, E6Color::Yellow);
            canvas.fill_rect(ix + 28, iy + 43,  4, 9, E6Color::Yellow);
            canvas.fill_rect(ix + 22, iy + 50, 10, 3, E6Color::Yellow);
            canvas.fill_rect(ix + 22, iy + 53,  4, 8, E6Color::Yellow);
        }
        WeatherCondition::Snow              => {
            draw_cloud(canvas, ix, iy);
            for &cx in &[18i32, 30, 42] {
                canvas.fill_rect(ix + cx - 4, iy + 47, 9, 3, E6Color::White);
                canvas.fill_rect(ix + cx - 1, iy + 43, 3, 9, E6Color::White);
            }
        }
        WeatherCondition::Fog               => {
            canvas.fill_rect(ix +  6, iy + 18, 52, 4, E6Color::Black);
            canvas.fill_rect(ix + 10, iy + 26, 44, 4, E6Color::Black);
            canvas.fill_rect(ix +  6, iy + 34, 52, 4, E6Color::Black);
            canvas.fill_rect(ix + 10, iy + 42, 44, 4, E6Color::Black);
            canvas.fill_rect(ix +  6, iy + 50, 52, 4, E6Color::Black);
        }
        WeatherCondition::Unknown           => {}
    }
}

// ── Battery icon + label ──────────────────────────────────────────────────────

fn draw_battery(canvas: &mut E6Canvas, batt: &BatteryInfo, right_edge: i32, top: i32, text_ascent: i32) {
    let pct_str = format!("{}%", batt.pct);
    let (text_w, _) = measure_text(&pct_str, BATT_FONT_PX, false);

    let text_x = right_edge - text_w;
    let icon_x = text_x - BATT_ICON_SEP - BATT_ICON_W;
    let icon_y = top + (text_ascent - BATT_ICON_H) / 2;

    // Body border (Black outline)
    canvas.fill_rect(icon_x, icon_y, BATT_ICON_BODY, BATT_ICON_H, E6Color::Black);
    // Interior (1px border)
    canvas.fill_rect(icon_x + 1, icon_y + 1, BATT_ICON_BODY - 2, BATT_ICON_H - 2, E6Color::White);
    // Nub on right side of body
    let nub_y = icon_y + (BATT_ICON_H - 4) / 2;
    canvas.fill_rect(icon_x + BATT_ICON_BODY, nub_y, BATT_ICON_NUB, 4, E6Color::Black);

    // Fill level: Blue=charging, Green≥25%, Yellow≥10%, Red<10%
    let fill_color = if batt.charging {
        E6Color::Blue
    } else if batt.pct >= 25 {
        E6Color::Green
    } else if batt.pct >= 10 {
        E6Color::Yellow
    } else {
        E6Color::Red
    };
    let fill_w = (BATT_ICON_BODY - 4) * batt.pct / 100;
    if fill_w > 0 {
        canvas.fill_rect(icon_x + 2, icon_y + 2, fill_w, BATT_ICON_H - 4, fill_color);
    }

    draw_text(canvas, text_x, top, &pct_str, BATT_FONT_PX, E6Color::Black, false);

    if batt.charging {
        // Lightning bolt: two right triangles, 14×6, centered in the 17×8 interior.
        // Upper: tip at top-center (bx+7), base at by+3 (8px, left half).
        // Lower: base at by+2 (7px, right half), tip at bottom-center (bx+7).
        // Triangles overlap at rows by+2 and by+3; those rows are drawn as unions.
        let bx = icon_x + 2;
        let by = icon_y + 2;
        canvas.fill_rect(bx + 7, by,      1,  1, E6Color::Yellow); // upper tip
        canvas.fill_rect(bx + 5, by + 1,  3,  1, E6Color::Yellow);
        canvas.fill_rect(bx + 3, by + 2, 11,  1, E6Color::Yellow); // upper 5 + lower 7, union
        canvas.fill_rect(bx,     by + 3, 12,  1, E6Color::Yellow); // upper 8 + lower 5, union
        canvas.fill_rect(bx + 7, by + 4,  3,  1, E6Color::Yellow);
        canvas.fill_rect(bx + 7, by + 5,  1,  1, E6Color::Yellow); // lower tip
    }
}

// ── Module impl ───────────────────────────────────────────────────────────────

impl Module for WeatherModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let guard = self.data.lock().unwrap();
        let Some(d) = *guard else { return };
        drop(guard);

        let battery = self.battery.lock().unwrap().clone();

        let cur_str  = format!("{}", d.current_f);
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
        let cur_y    = top_y + (block_h - cur_a) / 2;

        // Battery zone: measured from region top, may push H/L down
        let batt_ascent      = measure_text("A", BATT_FONT_PX, false).1;
        let batt_zone_bottom = if battery.is_some() {
            region.y + BATT_TOP_PAD + batt_ascent + BATT_GAP
        } else {
            0
        };
        let hl_y = (top_y + (block_h - hl_total) / 2).max(batt_zone_bottom);

        let icon_x = cur_x - ICON_GAP - ICON_SIZE;
        let icon_y = top_y + (block_h - ICON_SIZE) / 2;

        // Clear the icon + temperature number area to white before drawing.
        // draw_icon_bg handles the 64×64 icon cell; this erase also covers the
        // wider temperature text area and the optional battery zone above it.
        let erase_x      = icon_x.max(region.x);
        let erase_top    = if battery.is_some() { region.y } else { top_y };
        let erase_bottom = (top_y + block_h).max(hl_y + hl_total);
        let erase_w      = (region.x + region.width) - erase_x;
        canvas.fill_rect(erase_x, erase_top, erase_w, erase_bottom - erase_top, E6Color::White);

        draw_weather_icon(canvas, icon_x, icon_y, d.condition);

        draw_text(canvas, cur_x,     cur_y,                      &cur_str,  CURRENT_SIZE_PX, E6Color::Green, true);
        draw_text(canvas, hl_x_high, hl_y,                       &high_str, HL_SIZE_PX,      E6Color::Green, false);
        draw_text(canvas, hl_x_low,  hl_y + hl_a + HL_ROW_GAP,  &low_str,  HL_SIZE_PX,      E6Color::Green, false);

        if let Some(ref batt) = battery {
            draw_battery(canvas, batt, hl_right, region.y + BATT_TOP_PAD, batt_ascent);
        }
    }
}
