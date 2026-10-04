mod font;
mod image;
mod location;
mod modules;
mod nws_cache;
mod renderer;
mod stock_creds;

use std::sync::Arc;
use std::time::Duration;
use tower_http::trace::TraceLayer;
use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use crate::image::{SCREEN_W, SCREEN_H};
use tokio::sync::RwLock;
use tracing_subscriber::{fmt, EnvFilter};
use chrono::{DateTime, Local, Timelike};

use nws_cache::NwsPointsCache;
use modules::battery::parse_battery_header;
use modules::clock::ClockModule;
use modules::icon_matrix::IconMatrixModule;
use modules::mangosched::MangoSchedModule;
use modules::rain::{RainModule, NearTermRain};
use modules::stock::StockModule;
use modules::weather::{WeatherModule, WeatherData};
use renderer::{render, full_screen, schedule_region, RenderedImage};

const SERVER_VERSION: &str = env!("GIT_VERSION");

// ── Significant-change tracking ───────────────────────────────────────────────

/// Everything that can make the screen out of date, sampled at one moment.
struct Inputs {
    weather:       Option<WeatherData>,
    near_rain:     NearTermRain,
    batt_pct:      Option<i32>,
    batt_charging: Option<bool>,
    weather_stale: bool,
    sched_rev:     u64,
}

fn sample_inputs(state: &AppState) -> Inputs {
    let battery = state.weather.peek_battery();
    Inputs {
        weather:       state.weather.peek(),
        near_rain:     state.rain.peek_near(),
        batt_pct:      battery.as_ref().map(|b| b.pct),
        batt_charging: battery.as_ref().map(|b| b.charging),
        weather_stale: state.weather.is_stale(),
        sched_rev:     state.sched.revision(),
    }
}

/// What the currently served image was drawn from.
struct DisplayedState {
    refresh_time:   DateTime<Local>,
    current_temp_f: i32,
    high_f:         i32,
    low_f:          i32,
    near_rain:      NearTermRain,
    batt_pct:       Option<i32>,
    batt_charging:  Option<bool>,
    weather_stale:  bool,
    sched_rev:      u64,
}

fn is_significant_change(displayed: &DisplayedState, cur: &Inputs, now: DateTime<Local>) -> bool {
    if now.signed_duration_since(displayed.refresh_time).num_minutes() > 60 {
        return true;
    }
    if let Some(w) = cur.weather {
        if (w.current_f - displayed.current_temp_f).abs() >= 2 { return true; }
        if (w.high_f - displayed.high_f).abs() >= 3 { return true; }
        if (w.low_f  - displayed.low_f).abs()  >= 3 { return true; }
    }
    if cur.near_rain != displayed.near_rain { return true; }
    // Battery: charging state changed, or charge level shifted ≥5%
    if cur.batt_charging != displayed.batt_charging { return true; }
    if let (Some(c), Some(p)) = (cur.batt_pct, displayed.batt_pct) {
        if (c - p).abs() >= 5 { return true; }
    }
    // Weather went stale (grey background) or recovered
    if cur.weather_stale != displayed.weather_stale { return true; }
    // The schedule changed (including its day labels rolling over at midnight)
    if cur.sched_rev != displayed.sched_rev { return true; }
    false
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    image:             RwLock<RenderedImage>,
    fw_version:        RwLock<String>,
    weather:           WeatherModule,
    rain:              RainModule,
    sched:             MangoSchedModule,
    stock:             StockModule,
    displayed:         RwLock<Option<DisplayedState>>,
    icon_matrix_mode:  bool,
}
type SharedState = Arc<AppState>;

async fn commit_displayed(state: &AppState, now: DateTime<Local>, inp: Inputs) {
    let mut guard = state.displayed.write().await;
    let prev = guard.as_ref();
    let (current_temp_f, high_f, low_f) = inp.weather
        .map(|w| (w.current_f, w.high_f, w.low_f))
        .or_else(|| prev.map(|d| (d.current_temp_f, d.high_f, d.low_f)))
        .unwrap_or((0, 0, 0));
    *guard = Some(DisplayedState {
        refresh_time: now,
        current_temp_f,
        high_f,
        low_f,
        near_rain:     inp.near_rain,
        batt_pct:      inp.batt_pct.or_else(|| prev.and_then(|d| d.batt_pct)),
        batt_charging: inp.batt_charging.or_else(|| prev.and_then(|d| d.batt_charging)),
        weather_stale: inp.weather_stale,
        sched_rev:     inp.sched_rev,
    });
}

// ── Render helper ─────────────────────────────────────────────────────────────

async fn do_render(state: &AppState, show_version: bool) -> RenderedImage {
    let fw_ver    = state.fw_version.read().await.clone();
    let clock     = ClockModule;
    let icon_mtrx = IconMatrixModule;

    if state.icon_matrix_mode {
        let modules: &[(&dyn crate::modules::Module, _)] = &[
            (&clock,      full_screen()),
            (&icon_mtrx,  schedule_region()),
        ];
        return render(modules, SERVER_VERSION, &fw_ver, show_version, &state.stock);
    }

    let modules: &[(&dyn crate::modules::Module, _)] = &[
        (&clock,         full_screen()),
        (&state.rain,    full_screen()),
        (&state.weather, full_screen()),
        (&state.sched,   schedule_region()),
    ];
    render(modules, SERVER_VERSION, &fw_ver, show_version, &state.stock)
}

// ── Ticker config ─────────────────────────────────────────────────────────────

fn load_tickers() -> Vec<String> {
    match std::fs::read_to_string("stock_tickers.txt") {
        Ok(content) => content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(e) => {
            eprintln!("could not read stock_tickers.txt: {e}");
            Vec::new()
        }
    }
}

// ── Background render task ────────────────────────────────────────────────────

async fn render_loop(state: SharedState) {
    loop {
        let now = Local::now();

        // Each module throttles itself (weather and rain every 5 minutes, schedule every 10).
        tokio::join!(state.weather.refresh(), state.rain.refresh(), state.sched.refresh());
        let inputs = sample_inputs(&state);

        let should_render = {
            let ds = state.displayed.read().await;
            match ds.as_ref() {
                None     => true,
                Some(ds) => is_significant_change(ds, &inputs, now),
            }
        };

        if should_render {
            state.stock.refresh().await;
            let image = do_render(&state, false).await;
            *state.image.write().await = image;
            let (current, high, low) = inputs.weather.map(|w| (w.current_f, w.high_f, w.low_f)).unwrap_or((0, 0, 0));
            commit_displayed(&state, now, inputs).await;
            tracing::info!(current, high, low, "screen refreshed");
        }

        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

// ── GET /api/image ────────────────────────────────────────────────────────────

async fn get_image(
    State(state): State<SharedState>,
    req: Request<axum::body::Body>,
) -> impl IntoResponse {
    let device_id = req.headers()
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Parse the battery header; the weather module holds the reading so the next render shows it
    let batt_info = req.headers()
        .get("x-battery")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_battery_header);
    if let Some(ref batt) = batt_info {
        tracing::info!(
            "battery {}% {}mV charging={}{} (device: {device_id})",
            batt.pct, batt.mv, batt.charging,
            batt.hrs.map(|h| format!(" {:.1}h", h)).unwrap_or_default()
        );
    }
    state.weather.update_battery(batt_info);

    // New firmware version → re-render immediately.  After a server restart the version is merely
    // learned again ("unknown" → X) and the screen is redrawn as usual; a change from a known
    // version means new firmware was flashed, so the SV/FW bar replaces the stock strip until the
    // next render.
    if let Some(new_fw) = req.headers()
        .get("x-firmware-version")
        .and_then(|v| v.to_str().ok())
    {
        let mut fw = state.fw_version.write().await;
        if fw.as_str() != new_fw {
            let first_seen = fw.as_str() == "unknown";
            tracing::info!("Firmware version updated: {:?} → {:?}", *fw, new_fw);
            *fw = new_fw.to_string();
            drop(fw);
            let inputs    = sample_inputs(&state);
            let new_image = do_render(&state, !first_seen).await;
            *state.image.write().await = new_image;
            commit_displayed(&state, Local::now(), inputs).await;
        }
    }

    let image      = state.image.read().await;
    let etag_value = format!("\"{}\"", image.etag);

    let client_etag = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let mut headers = HeaderMap::new();
    let now  = Local::now();
    let h    = now.hour();
    let m    = now.minute();
    let s    = now.second();
    let poll_secs: u64 = if h >= 23 || h < 5 || (h == 5 && m < 45) {
        // 11:00pm – 5:44:59am: deep-night long poll
        3600
    } else if h > 6 || (h == 6 && m >= 45) {
        // 6:45am – 10:59pm: normal fast poll
        300
    } else {
        // 5:45am – 6:44:59am: count down to 6:45am wake-up
        let now_secs:  u32 = h * 3600 + m * 60 + s;
        let wake_secs: u32 = 6 * 3600 + 45 * 60; // 24300
        u64::from(wake_secs.saturating_sub(now_secs)).max(1)
    };
    add_common_headers(&mut headers, &etag_value, poll_secs);

    if client_etag == etag_value {
        tracing::info!("GET /api/image → 304 (device: {device_id})");
        return (StatusCode::NOT_MODIFIED, headers, vec![]).into_response();
    }

    tracing::info!("GET /api/image → 200 {} bytes (device: {device_id})", image.packed.len());
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    (StatusCode::OK, headers, image.packed.clone()).into_response()
}

fn add_common_headers(headers: &mut HeaderMap, etag: &str, poll_secs: u64) {
    headers.insert(header::ETAG,          HeaderValue::from_str(etag).unwrap());
    headers.insert("X-Poll-Interval",     HeaderValue::from_str(&poll_secs.to_string()).unwrap());
    headers.insert("X-Server-Time",       HeaderValue::from_str(&chrono::Utc::now().timestamp().to_string()).unwrap());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // The display deep-sleeps with its radio off right after a poll and never closes its TCP
    // connection, so without this each full-image fetch leaves a socket (and file descriptor)
    // open on the server forever.  Closing from our side after the response frees it.
    headers.insert(header::CONNECTION,    HeaderValue::from_static("close"));
}

// ── Browser preview server (port 17654) ──────────────────────────────────────

// E6 nibble → (R, G, B).  Indices 4, 7–15 are unused; they map to black.
const E6_PALETTE: [(u8, u8, u8); 16] = {
    let mut p = [(0u8, 0u8, 0u8); 16];
    p[0x1] = (0xFF, 0xFF, 0xFF); // White
    p[0x2] = (0xFF, 0xD7, 0x00); // Yellow
    p[0x3] = (0xCC, 0x22, 0x00); // Red
    p[0x5] = (0x00, 0x55, 0xCC); // Blue
    p[0x6] = (0x00, 0x99, 0x00); // Green
    p
};

fn packed_to_png(packed: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((SCREEN_W * SCREEN_H * 3) as usize);
    // The packed buffer runs from the last pixel to the first: walk it backwards, low nibble first.
    for &byte in packed.iter().rev() {
        let (r, g, b) = E6_PALETTE[(byte & 0x0F) as usize];
        rgb.push(r); rgb.push(g); rgb.push(b);
        let (r, g, b) = E6_PALETTE[(byte >> 4) as usize];
        rgb.push(r); rgb.push(g); rgb.push(b);
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, SCREEN_W as u32, SCREEN_H as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap()
        .write_image_data(&rgb).unwrap();
    out
}

static PREVIEW_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>PhotoPainter Preview</title>
<style>
  html, body { margin: 0; padding: 0; background: #111; }
  img { width: 100%; height: auto; display: block; image-rendering: pixelated; }
</style>
</head>
<body>
<img id="frame" src="/image.png">
<script>
  setInterval(function() {
    document.getElementById("frame").src = "/image.png?" + Date.now();
  }, 60000);
</script>
</body>
</html>"#;

async fn preview_page() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"),
         (header::CACHE_CONTROL, "no-store")],
        PREVIEW_HTML,
    )
}

async fn preview_image(State(state): State<SharedState>) -> impl IntoResponse {
    let png = packed_to_png(&state.image.read().await.packed);
    (
        [(header::CONTENT_TYPE, "image/png"),
         (header::CACHE_CONTROL, "no-store")],
        png,
    )
}

async fn run_preview_server(state: SharedState) {
    let app = Router::new()
        .route("/",          get(preview_page))
        .route("/image.png", get(preview_image))
        .with_state(state);
    let addr = "0.0.0.0:17654";
    tracing::info!("preview server listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let tickers   = load_tickers();
    let nws_cache = Arc::new(NwsPointsCache::new());
    let client    = reqwest::Client::builder()
        .user_agent("PhotoPainter/1.0 (mangocats@gmail.com)")
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build HTTP client");

    let weather = WeatherModule::new(client.clone(), Arc::clone(&nws_cache));
    let rain    = RainModule::new(client.clone(), Arc::clone(&nws_cache));
    let sched   = MangoSchedModule::new();
    let stock   = StockModule::new(tickers, client);

    let icon_matrix_mode = std::env::var("ICON_MATRIX").is_ok();
    if icon_matrix_mode {
        tracing::info!("ICON_MATRIX mode: schedule replaced with icon grid");
    }

    let state: SharedState = Arc::new(AppState {
        image:      RwLock::new(RenderedImage { packed: Vec::new(), etag: String::new() }),
        fw_version: RwLock::new("unknown".to_string()),
        weather,
        rain,
        sched,
        stock,
        displayed:  RwLock::new(None),
        icon_matrix_mode,
    });

    // The first render carries the SV/FW bar; every render after it (render_loop) shows the stock strip
    let initial = do_render(&state, true).await;
    *state.image.write().await = initial;

    tokio::spawn(render_loop(Arc::clone(&state)));
    tokio::spawn(run_preview_server(Arc::clone(&state)));

    let app = Router::new()
        .route("/api/image", get(get_image))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = "0.0.0.0:7654";
    tracing::info!("listening on {addr} (server version: {SERVER_VERSION})");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::image::{E6Canvas, E6Color};
    use crate::modules::Module;

    /// Renders the schedule region from the synthetic fixture to a PNG for eyeballing the layout.
    #[test]
    fn render_schedule_previews() {
        let html = include_str!("../testdata/mangosched_calendar.html");
        let days = modules::mangosched::parse_calendar(html).unwrap();
        let module = MangoSchedModule::with_days(days);
        let mut canvas = E6Canvas::new(E6Color::White);
        module.render(&mut canvas, schedule_region());
        std::fs::write("/tmp/ms_preview.png", packed_to_png(&canvas.pack())).unwrap();
    }

    #[test]
    fn schedule_and_stale_weather_changes_trigger_a_render() {
        let now = Local::now();
        let displayed = DisplayedState {
            refresh_time: now, current_temp_f: 70, high_f: 80, low_f: 60, near_rain: NearTermRain::None,
            batt_pct: Some(50), batt_charging: Some(false), weather_stale: false, sched_rev: 3,
        };
        let same = || Inputs {
            weather: Some(WeatherData { current_f: 70, high_f: 80, low_f: 60, ..Default::default() }),
            near_rain: NearTermRain::None, batt_pct: Some(50), batt_charging: Some(false),
            weather_stale: false, sched_rev: 3,
        };
        assert!(!is_significant_change(&displayed, &same(), now), "nothing changed");
        let mut i = same(); i.sched_rev = 4;
        assert!(is_significant_change(&displayed, &i, now), "a schedule change must re-render");
        let mut i = same(); i.weather_stale = true;
        assert!(is_significant_change(&displayed, &i, now), "weather going stale must re-render");
    }

    /// Renders the weather block fresh and stale (speckled background) for eyeballing.
    #[test]
    fn render_weather_stale_previews() {
        use std::time::Duration;
        use crate::modules::battery::BatteryInfo;
        let data = WeatherData { current_f: 76, high_f: 81, low_f: 73, ..Default::default() };
        let batt = Some(BatteryInfo { pct: 57, mv: 3800, hrs: None, charging: false });
        for (name, age) in [("fresh", 60u64), ("stale", 31 * 60)] {
            let w = WeatherModule::with_data(data, Duration::from_secs(age), batt.clone());
            assert_eq!(w.is_stale(), name == "stale");
            let mut canvas = E6Canvas::new(E6Color::White);
            ClockModule.render(&mut canvas, full_screen());
            w.render(&mut canvas, full_screen());
            std::fs::write(format!("/tmp/weather_{name}.png"), packed_to_png(&canvas.pack())).unwrap();
        }
    }
}
