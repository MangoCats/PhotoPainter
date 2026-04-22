use std::sync::Mutex;
use std::time::Duration;
use chrono::{Utc, Local, Timelike};
use crate::font::{draw_text, measure_text};
use crate::image::{E6Canvas, E6Color};
use crate::location;
use super::{Module, Rect};

const SIZE_PX:          f32 = 28.0;
const MARGIN:           i32 = 8;
const LINE_GAP:         i32 = 4;
const CLOCK_SIZE_PX:    i32 = 24;   // must match clock::SIZE_PX
const CLOCK_MARGIN:     i32 = 4;    // must match clock::MARGIN
// Maximum pixel width for rain text — keeps it left of the temperature block.
// Weather temp display starts at ~x=489 for 3-digit temperatures (worst case).
const RAIN_MAX_W:       i32 = 500;
const RAIN_THRESHOLD:   f64 = 0.001;
const FORECAST_HOURS:   f64 = 168.0;
const NEAR_TERM_HOURS:  f64 = 6.0;

/// Discrete near-term rain state used for significant-change detection.
/// Rates stored as milliinches/hr and times as tenth-hours for stable equality.
#[derive(Clone, PartialEq)]
pub enum NearTermRain {
    None,
    Active   { rate_milliinches: i32 },
    Imminent { rate_milliinches: i32, tenth_hours: i32 },
}

pub struct RainData {
    pub line1: String,
    pub line2: Option<String>,
    pub near:  NearTermRain,
}

struct GridCache {
    url: String,
}

pub struct RainModule {
    data:   Mutex<Option<RainData>>,
    cache:  Mutex<Option<GridCache>>,
    client: reqwest::Client,
}

impl RainModule {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent("PhotoPainter/1.0 (github.com/photopainter)")
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");
        Self { data: Mutex::new(None), cache: Mutex::new(None), client }
    }

    pub fn peek_near(&self) -> NearTermRain {
        self.data.lock().unwrap()
            .as_ref()
            .map(|d| d.near.clone())
            .unwrap_or(NearTermRain::None)
    }

    pub async fn refresh(&self) {
        match self.fetch().await {
            Ok(d)  => *self.data.lock().unwrap() = Some(d),
            Err(e) => tracing::warn!("rain fetch failed: {e}"),
        }
    }

    async fn grid_url(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        {
            let g = self.cache.lock().unwrap();
            if let Some(c) = g.as_ref() { return Ok(c.url.clone()); }
        }
        let url = format!("https://api.weather.gov/points/{:.4},{:.4}", location::LAT, location::LON);
        let body: serde_json::Value = self.client.get(&url).send().await?.json().await?;
        let grid = body["properties"]["forecastGridData"]
            .as_str().ok_or("missing forecastGridData")?.to_string();
        *self.cache.lock().unwrap() = Some(GridCache { url: grid.clone() });
        Ok(grid)
    }

    async fn fetch(&self) -> Result<RainData, Box<dyn std::error::Error + Send + Sync>> {
        let grid_url = self.grid_url().await?;
        let body: serde_json::Value = self.client.get(&grid_url).send().await?.json().await?;

        let qpf = &body["properties"]["quantitativePrecipitation"];
        let uom  = qpf["uom"].as_str().unwrap_or("wmoUnit:m");
        let vals = qpf["values"].as_array().ok_or("missing QPF values")?;

        let now = Utc::now();
        let mut rain_periods: Vec<(f64, f32)> = Vec::new(); // (start_offset_hours, rate_in_hr)

        for v in vals {
            let valid_time = v["validTime"].as_str().ok_or("missing validTime")?;
            let (ts_str, dur_str) = valid_time.split_once('/').ok_or("bad validTime")?;

            let start: chrono::DateTime<chrono::FixedOffset> = ts_str.parse()?;
            let start_utc  = start.with_timezone(&Utc);
            let start_off  = (start_utc - now).num_seconds() as f64 / 3600.0;
            let dur_hours  = parse_duration_hours(dur_str).ok_or("bad duration")?;
            let end_off    = start_off + dur_hours;

            if end_off <= 0.0 || start_off >= FORECAST_HOURS { continue; }

            let raw_val  = v["value"].as_f64().unwrap_or(0.0);
            let inches   = match uom {
                "wmoUnit:m"  => raw_val * 39.3701,
                "wmoUnit:mm" => raw_val * 0.0393701,
                _            => raw_val,  // assume inches
            };
            let rate     = (inches / dur_hours) as f32;

            if (rate as f64) > RAIN_THRESHOLD {
                rain_periods.push((start_off, rate));
            }
        }

        rain_periods.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        Ok(compute_forecast(rain_periods))
    }
}

fn parse_duration_hours(s: &str) -> Option<f64> {
    let s = s.strip_prefix('P')?;
    let (day_part, time_part) = if let Some(pos) = s.find('T') {
        (&s[..pos], Some(&s[pos + 1..]))
    } else {
        (s, None)
    };
    let days: f64 = if day_part.is_empty() {
        0.0
    } else {
        day_part.strip_suffix('D')?.parse().ok()?
    };
    let hours: f64 = match time_part {
        None | Some("") => 0.0,
        Some(t)         => t.strip_suffix('H')?.parse().ok()?,
    };
    Some(days * 24.0 + hours)
}

fn category_name(rate: f32) -> &'static str {
    if rate < 0.10 { "Light" }
    else if rate < 0.30 { "Moderate" }
    else if rate < 2.0  { "Heavy" }
    else                 { "Extreme" }
}

fn category_rank(rate: f32) -> u8 {
    if rate < 0.10 { 0 }
    else if rate < 0.30 { 1 }
    else if rate < 2.0  { 2 }
    else                 { 3 }
}

/// Returns a human-readable start-time description for a rain period that
/// begins `start_off_hours` hours from now (must be > 0).
///
/// < 91 min          → "N minutes"
/// same calendar day or before 6 AM next morning → "N.N hours"
/// tomorrow (≥ 6 AM) → "Tomorrow @ H:MMam"
/// later             → "DayOfWeek @ H:MMam"
fn format_start(start_off_hours: f64) -> String {
    let now   = Local::now();
    let start = now + chrono::Duration::seconds((start_off_hours * 3600.0).round() as i64);
    let mins  = (start_off_hours * 60.0).round() as i64;

    if mins < 91 {
        return if mins == 1 { "1 minute".to_string() } else { format!("{mins} minutes") };
    }

    let today    = now.date_naive();
    let tomorrow = today.succ_opt().unwrap_or(today);
    let sdate    = start.date_naive();
    let shour    = start.hour();

    if sdate == today || (sdate == tomorrow && shour < 6) {
        return format!("{:.1} hours", start_off_hours);
    }

    let (_, h12) = start.hour12();
    let ampm     = if shour < 12 { "AM" } else { "PM" };
    let time_str = format!("{h12}:{:02}{ampm}", start.minute());

    if sdate == tomorrow {
        format!("Tomorrow @ {time_str}")
    } else {
        format!("{} @ {time_str}", start.format("%A"))
    }
}

fn compute_forecast(rain_periods: Vec<(f64, f32)>) -> RainData {
    let Some(&(first_start, first_rate)) = rain_periods.first() else {
        return RainData {
            line1: "No rain for 7 days.".to_string(),
            line2: None,
            near:  NearTermRain::None,
        };
    };

    let line1 = if first_start <= 0.0 {
        format!("Raining {:.2} in/hr.", first_rate)
    } else {
        format!("{} {:.2} starting {}.", category_name(first_rate), first_rate, format_start(first_start))
    };

    let near = if first_start <= 0.0 {
        NearTermRain::Active { rate_milliinches: (first_rate * 1000.0).round() as i32 }
    } else if first_start <= NEAR_TERM_HOURS {
        NearTermRain::Imminent {
            rate_milliinches: (first_rate * 1000.0).round() as i32,
            tenth_hours:      (first_start * 10.0).round() as i32,
        }
    } else {
        NearTermRain::None
    };

    // Second line: earliest period of a heavier category than the first
    let first_rank = category_rank(first_rate);
    let line2 = rain_periods.iter()
        .skip(1)
        .find(|&&(start, rate)| category_rank(rate) > first_rank && start > first_start)
        .map(|&(start, rate)| {
            if start <= 0.0 {
                format!("{} {:.2} in/hr now.", category_name(rate), rate)
            } else {
                format!("{} {:.2} starting {}.", category_name(rate), rate, format_start(start))
            }
        });

    RainData { line1, line2, near }
}

/// Word-wrap `text` so no line exceeds `max_w` pixels at SIZE_PX.
fn word_wrap(text: &str, max_w: i32) -> Vec<String> {
    let mut lines  = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure_text(&candidate, SIZE_PX, false).0 <= max_w {
            current = candidate;
        } else {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            current = word.to_string();
        }
    }
    if !current.is_empty() { lines.push(current); }
    lines
}

impl Module for RainModule {
    fn render(&self, canvas: &mut E6Canvas, region: Rect) {
        let guard = self.data.lock().unwrap();
        let Some(d) = guard.as_ref() else { return };
        let line1 = d.line1.clone();
        let line2 = d.line2.clone();
        drop(guard);

        let clock_ascent = measure_text("A", CLOCK_SIZE_PX as f32, false).1;
        let rain_ascent  = measure_text("A", SIZE_PX, false).1;
        let line_h       = rain_ascent + LINE_GAP;

        let mut y = region.y + CLOCK_MARGIN + clock_ascent + LINE_GAP;

        for sub in word_wrap(&line1, RAIN_MAX_W) {
            draw_text(canvas, region.x + MARGIN, y, &sub, SIZE_PX, E6Color::Blue, false);
            y += line_h;
        }
        if let Some(l2) = line2 {
            for sub in word_wrap(&l2, RAIN_MAX_W) {
                draw_text(canvas, region.x + MARGIN, y, &sub, SIZE_PX, E6Color::Blue, false);
                y += line_h;
            }
        }
    }

    fn data_refresh_interval(&self) -> Duration { Duration::from_secs(300) }
    fn suggested_poll_interval(&self) -> Option<Duration> { None }
}
