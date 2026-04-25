mod font;
mod gcal_creds;
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
use chrono::{DateTime, Local};

use nws_cache::NwsPointsCache;
use modules::battery::parse_battery_header;
use modules::clock::ClockModule;
use modules::gcal::GCalModule;
use modules::icon_matrix::IconMatrixModule;
use modules::rain::{RainModule, NearTermRain};
use modules::stock::StockModule;
use modules::weather::{WeatherModule, WeatherData};
use renderer::{render, full_screen, gcal_region, RenderedImage};

const SERVER_VERSION: &str = env!("GIT_VERSION");

// ── Significant-change tracking ───────────────────────────────────────────────

struct DisplayedState {
    refresh_time:   DateTime<Local>,
    current_temp_f: i32,
    high_f:         i32,
    low_f:          i32,
    near_rain:      NearTermRain,
    batt_pct:       Option<i32>,
    batt_charging:  Option<bool>,
}

fn is_significant_change(
    displayed:    &DisplayedState,
    weather:      Option<WeatherData>,
    near_rain:    NearTermRain,
    batt_pct:     Option<i32>,
    batt_charging: Option<bool>,
    now:          DateTime<Local>,
) -> bool {
    if now.signed_duration_since(displayed.refresh_time).num_minutes() > 60 {
        return true;
    }
    if let Some(w) = weather {
        if (w.current_f - displayed.current_temp_f).abs() >= 2 { return true; }
        if (w.high_f - displayed.high_f).abs() >= 3 { return true; }
        if (w.low_f  - displayed.low_f).abs()  >= 3 { return true; }
    }
    if near_rain != displayed.near_rain { return true; }
    // Battery: charging state changed, or charge level shifted ≥5%
    if batt_charging != displayed.batt_charging { return true; }
    if let (Some(cur), Some(prev)) = (batt_pct, displayed.batt_pct) {
        if (cur - prev).abs() >= 5 { return true; }
    }
    false
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    image:             RwLock<RenderedImage>,
    fw_version:        RwLock<String>,
    weather:           WeatherModule,
    rain:              RainModule,
    gcal:              GCalModule,
    stock:             StockModule,
    displayed:         RwLock<Option<DisplayedState>>,
    icon_matrix_mode:  bool,
}
type SharedState = Arc<AppState>;

async fn commit_displayed(
    state:         &AppState,
    now:           DateTime<Local>,
    weather:       Option<WeatherData>,
    near_rain:     NearTermRain,
    batt_pct:      Option<i32>,
    batt_charging: Option<bool>,
) {
    let mut guard = state.displayed.write().await;
    let prev = guard.as_ref();
    let (current_temp_f, high_f, low_f) = weather
        .map(|w| (w.current_f, w.high_f, w.low_f))
        .or_else(|| prev.map(|d| (d.current_temp_f, d.high_f, d.low_f)))
        .unwrap_or((0, 0, 0));
    *guard = Some(DisplayedState {
        refresh_time: now,
        current_temp_f,
        high_f,
        low_f,
        near_rain,
        batt_pct:      batt_pct.or_else(|| prev.and_then(|d| d.batt_pct)),
        batt_charging: batt_charging.or_else(|| prev.and_then(|d| d.batt_charging)),
    });
}

// ── Render helper ─────────────────────────────────────────────────────────────

async fn do_render(state: &AppState, show_version: bool) -> RenderedImage {
    let fw_ver     = state.fw_version.read().await.clone();
    let clock      = ClockModule;
    let icon_mtrx  = IconMatrixModule;
    let modules: &[(&dyn crate::modules::Module, _)] = if state.icon_matrix_mode {
        &[
            (&clock,      full_screen()),
            (&icon_mtrx,  gcal_region()),
        ]
    } else {
        &[
            (&clock,         full_screen()),
            (&state.rain,    full_screen()),
            (&state.weather, full_screen()),
            (&state.gcal,    gcal_region()),
        ]
    };
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
        tokio::join!(state.weather.refresh(), state.rain.refresh(), state.gcal.refresh());
        let now       = Local::now();
        let weather   = state.weather.peek();
        let near_rain = state.rain.peek_near();
        let battery   = state.weather.peek_battery();
        let batt_pct      = battery.as_ref().map(|b| b.pct);
        let batt_charging = battery.as_ref().map(|b| b.charging);

        let should_render = {
            let ds = state.displayed.read().await;
            match ds.as_ref() {
                None     => true,
                Some(ds) => is_significant_change(ds, weather, near_rain.clone(), batt_pct, batt_charging, now),
            }
        };

        if should_render {
            state.stock.refresh().await;
            let image = do_render(&state, false).await;
            *state.image.write().await = image;
            commit_displayed(&state, now, weather, near_rain, batt_pct, batt_charging).await;
            tracing::info!(
                current = weather.map(|w| w.current_f).unwrap_or(0),
                high    = weather.map(|w| w.high_f).unwrap_or(0),
                low     = weather.map(|w| w.low_f).unwrap_or(0),
                "screen refreshed"
            );
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

    // Parse battery header; update weather module so next render reflects it
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

    // Firmware version change → re-render immediately with updated version string
    if let Some(new_fw) = req.headers()
        .get("x-firmware-version")
        .and_then(|v| v.to_str().ok())
    {
        let mut fw = state.fw_version.write().await;
        if fw.as_str() != new_fw {
            tracing::info!("Firmware version updated: {:?} → {:?}", *fw, new_fw);
            *fw = new_fw.to_string();
            drop(fw);
            let now       = Local::now();
            let weather   = state.weather.peek();
            let near_rain = state.rain.peek_near();
            let batt_pct      = state.weather.peek_battery().as_ref().map(|b| b.pct);
            let batt_charging = state.weather.peek_battery().as_ref().map(|b| b.charging);
            let new_image = do_render(&state, false).await;
            *state.image.write().await = new_image;
            commit_displayed(&state, now, weather, near_rain, batt_pct, batt_charging).await;
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
    add_common_headers(&mut headers, &etag_value, 60);

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
    // 180° rotation: iterate bytes in reverse, lo nibble before hi within each byte.
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
        .user_agent("PhotoPainter/1.0 (github.com/photopainter)")
        .timeout(Duration::from_secs(15))
        .build()
        .expect("failed to build HTTP client");

    let weather = WeatherModule::new(client.clone(), Arc::clone(&nws_cache));
    let rain    = RainModule::new(client.clone(), Arc::clone(&nws_cache));
    let gcal    = GCalModule::new(client.clone());
    let stock   = StockModule::new(tickers, client);

    let icon_matrix_mode = std::env::var("ICON_MATRIX").is_ok();
    if icon_matrix_mode {
        tracing::info!("ICON_MATRIX mode: gcal replaced with icon grid");
    }

    let state: SharedState = Arc::new(AppState {
        image:      RwLock::new(RenderedImage { packed: Vec::new(), etag: String::new() }),
        fw_version: RwLock::new("unknown".to_string()),
        weather,
        rain,
        gcal,
        stock,
        displayed:  RwLock::new(None),
        icon_matrix_mode,
    });

    // Initial render shows the version bar once; render_loop always shows the stock strip
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
