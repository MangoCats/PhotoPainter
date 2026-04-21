mod font;
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

use modules::clock::ClockModule;
use modules::weather::WeatherModule;
use renderer::{render, full_screen, RenderedImage};

const SERVER_VERSION: &str = env!("GIT_VERSION");

struct AppState {
    image:      RwLock<RenderedImage>,
    fw_version: RwLock<String>,
    weather:    WeatherModule,
}
type SharedState = Arc<AppState>;

// ── Background render task ────────────────────────────────────────────────────
async fn render_loop(state: SharedState) {
    let clock = ClockModule;
    loop {
        state.weather.refresh().await;
        let fw_ver = state.fw_version.read().await.clone();
        let image = render(
            &[(&clock, full_screen()), (&state.weather, full_screen())],
            SERVER_VERSION,
            &fw_ver,
        );
        *state.image.write().await = image;
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

    // Re-render immediately if firmware version has changed
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
            let clock = ClockModule;
            let new_image = render(
                &[(&clock, full_screen()), (&state.weather, full_screen())],
                SERVER_VERSION,
                &fw_str,
            );
            *state.image.write().await = new_image;
        }
    }

    let image = state.image.read().await;
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
    let initial = render(&[(&clock, full_screen()), (&weather, full_screen())], SERVER_VERSION, "unknown");
    let state: SharedState = Arc::new(AppState {
        image:      RwLock::new(initial),
        fw_version: RwLock::new("unknown".to_string()),
        weather,
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
