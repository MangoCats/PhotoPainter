mod image;
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
use renderer::{render, full_screen, RenderedImage};

type SharedState = Arc<RwLock<RenderedImage>>;

// ── Background render task ────────────────────────────────────────────────────
async fn render_loop(state: SharedState) {
    let clock = ClockModule;
    loop {
        let image = render(&[(&clock, full_screen())]);
        *state.write().await = image;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

// ── GET /api/image ────────────────────────────────────────────────────────────
async fn get_image(
    State(state): State<SharedState>,
    req: Request<axum::body::Body>,
) -> impl IntoResponse {
    let image = state.read().await;
    let etag_value = format!("\"{}\"", image.etag);

    // Check If-None-Match
    let client_etag = req
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let mut headers = HeaderMap::new();
    add_common_headers(&mut headers, &etag_value, 60);

    if client_etag == etag_value {
        return (StatusCode::NOT_MODIFIED, headers, vec![]).into_response();
    }

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    (StatusCode::OK, headers, image.packed.clone()).into_response()
}

fn add_common_headers(headers: &mut HeaderMap, etag: &str, poll_secs: u64) {
    headers.insert(header::ETAG,        HeaderValue::from_str(etag).unwrap());
    headers.insert("X-Poll-Interval",   HeaderValue::from_str(&poll_secs.to_string()).unwrap());
    headers.insert("X-Server-Time",     HeaderValue::from_str(&chrono::Utc::now().timestamp().to_string()).unwrap());
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

// ── Main ──────────────────────────────────────────────────────────────────────
#[tokio::main]
async fn main() {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let clock = ClockModule;
    let initial = render(&[(&clock, full_screen())]);
    let state: SharedState = Arc::new(RwLock::new(initial));

    tokio::spawn(render_loop(Arc::clone(&state)));

    let app = Router::new()
        .route("/api/image", get(get_image))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = "0.0.0.0:7654";
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
