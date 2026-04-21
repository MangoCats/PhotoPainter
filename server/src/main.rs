mod font;
mod gcal_creds;
mod image;
mod location;
mod modules;
mod renderer;

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
use tokio::sync::RwLock;
use tracing_subscriber::{fmt, EnvFilter};
use chrono::{DateTime, Local};

use modules::clock::ClockModule;
use modules::gcal::GCalModule;
use modules::rain::{RainModule, NearTermRain};
use modules::weather::{WeatherModule, WeatherData};
use renderer::{render, full_screen, RenderedImage};

const SERVER_VERSION: &str = env!("GIT_VERSION");

// ── Significant-change tracking ───────────────────────────────────────────────

/// Values as they appeared at the most recent screen refresh.
struct DisplayedState {
    refresh_time:   DateTime<Local>,
    current_temp_f: i32,
    high_f:         i32,
    low_f:          i32,
    near_rain:      NearTermRain,
}

/// Returns true if any data element has changed enough to warrant a repaint.
fn is_significant_change(
    displayed:  &DisplayedState,
    weather:    Option<WeatherData>,
    near_rain:  NearTermRain,
    now:        DateTime<Local>,
) -> bool {
    // Time: more than one hour since last refresh
    if now.signed_duration_since(displayed.refresh_time).num_minutes() > 60 {
        return true;
    }
    if let Some(w) = weather {
        // Current temperature: two or more degrees
        if (w.current_f - displayed.current_temp_f).abs() >= 2 { return true; }
        // Forecast high or low: three or more degrees
        if (w.high_f - displayed.high_f).abs() >= 3 { return true; }
        if (w.low_f  - displayed.low_f).abs()  >= 3 { return true; }
    }
    // Near-term rain (≤6 hr window) changed
    if near_rain != displayed.near_rain { return true; }
    false
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    image:      RwLock<RenderedImage>,
    fw_version: RwLock<String>,
    weather:    WeatherModule,
    rain:       RainModule,
    gcal:       GCalModule,
    displayed:  RwLock<Option<DisplayedState>>,
}
type SharedState = Arc<AppState>;

/// Commit a fresh render to `state.displayed`, preserving last-known temp values
/// if weather data was not available at render time.
async fn commit_displayed(
    state:     &AppState,
    now:       DateTime<Local>,
    weather:   Option<WeatherData>,
    near_rain: NearTermRain,
) {
    let mut guard = state.displayed.write().await;
    let prev = guard.as_ref();
    *guard = Some(DisplayedState {
        refresh_time:   now,
        current_temp_f: weather.map(|w| w.current_f)
            .or_else(|| prev.map(|d| d.current_temp_f))
            .unwrap_or(0),
        high_f: weather.map(|w| w.high_f)
            .or_else(|| prev.map(|d| d.high_f))
            .unwrap_or(0),
        low_f: weather.map(|w| w.low_f)
            .or_else(|| prev.map(|d| d.low_f))
            .unwrap_or(0),
        near_rain,
    });
}

// ── Background render task ────────────────────────────────────────────────────

async fn render_loop(state: SharedState) {
    let clock = ClockModule;
    loop {
        tokio::join!(state.weather.refresh(), state.rain.refresh(), state.gcal.refresh());
        let now       = Local::now();
        let weather   = state.weather.peek();
        let near_rain = state.rain.peek_near();

        let should_render = {
            let ds = state.displayed.read().await;
            match ds.as_ref() {
                None     => true,   // never rendered yet
                Some(ds) => is_significant_change(ds, weather, near_rain.clone(), now),
            }
        };

        if should_render {
            let fw_ver = state.fw_version.read().await.clone();
            let image  = render(
                &[(&clock, full_screen()), (&state.weather, full_screen()), (&state.rain, full_screen()), (&state.gcal, full_screen())],
                SERVER_VERSION,
                &fw_ver,
            );
            *state.image.write().await = image;
            commit_displayed(&state, now, weather, near_rain).await;
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

    // Firmware version change is always a significant change — re-render immediately
    if let Some(new_fw) = req.headers()
        .get("x-firmware-version")
        .and_then(|v| v.to_str().ok())
    {
        let mut fw = state.fw_version.write().await;
        if fw.as_str() != new_fw {
            tracing::info!("Firmware version updated: {:?} → {:?}", *fw, new_fw);
            *fw = new_fw.to_string();
            let fw_str = fw.clone();
            drop(fw);
            let clock   = ClockModule;
            let now     = Local::now();
            let weather = state.weather.peek();
            let near_rain = state.rain.peek_near();
            let new_image = render(
                &[(&clock, full_screen()), (&state.weather, full_screen()), (&state.rain, full_screen()), (&state.gcal, full_screen())],
                SERVER_VERSION,
                &fw_str,
            );
            *state.image.write().await = new_image;
            commit_displayed(&state, now, weather, near_rain).await;
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

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let clock   = ClockModule;
    let weather = WeatherModule::new();
    let rain    = RainModule::new();
    let gcal    = GCalModule::new();
    let initial = render(&[(&clock, full_screen()), (&weather, full_screen()), (&rain, full_screen()), (&gcal, full_screen())], SERVER_VERSION, "unknown");
    let state: SharedState = Arc::new(AppState {
        image:      RwLock::new(initial),
        fw_version: RwLock::new("unknown".to_string()),
        weather,
        rain,
        gcal,
        displayed:  RwLock::new(None),  // forces a render on first loop iteration
    });

    tokio::spawn(render_loop(Arc::clone(&state)));

    let app = Router::new()
        .route("/api/image", get(get_image))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = "0.0.0.0:7654";
    tracing::info!("listening on {addr} (server version: {SERVER_VERSION})");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
