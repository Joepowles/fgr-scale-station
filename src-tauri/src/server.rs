//! The local HTTP server the window gets pictures from, because <img> and
//! <video> want a URL, not a buffer:
//!
//!   http://127.0.0.1:<port>/stream?hd=1   fragmented MP4 of the chosen stream
//!   http://127.0.0.1:<port>/snapshot?hd=1 one JPEG
//!   http://127.0.0.1:<port>/photo/<name>  a history photo
//!
//! Bound to 127.0.0.1 on a port the OS picks, so nothing off this PC can
//! reach it.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tauri::{AppHandle, Manager};

use crate::camera::{self, CameraSettings};
use crate::AppState;

/// Binds now, so the port is known before the window asks for it, and
/// serves in the background.
pub fn start(app: AppHandle) -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let router = Router::new().route("/stream", get(stream)).route("/snapshot", get(snapshot)).route("/photo/{name}", get(photo)).with_state(app);
    tauri::async_runtime::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("listener from std");
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
}

#[derive(serde::Deserialize)]
struct Which {
    hd: Option<String>,
}

/// The stream URL the settings name, login included.
fn camera_url(app: &AppHandle, which: &Which) -> Option<String> {
    let settings: CameraSettings = app.state::<AppState>().settings.lock().unwrap().section("camera");
    settings.url(which.hd.as_deref() == Some("1"))
}

async fn stream(State(app): State<AppHandle>, Query(which): Query<Which>) -> Response {
    let Some(url) = camera_url(&app, &which) else { return (StatusCode::NOT_FOUND, "no camera").into_response() };
    if !camera::ffmpeg::available() {
        return (StatusCode::SERVICE_UNAVAILABLE, "ffmpeg missing").into_response();
    }
    let body = Body::from_stream(app.state::<AppState>().streams.watch(&url));
    ([(header::CONTENT_TYPE, "video/mp4"), (header::CACHE_CONTROL, "no-cache, no-store")], body).into_response()
}

async fn snapshot(State(app): State<AppHandle>, Query(which): Query<Which>) -> Response {
    let Some(url) = camera_url(&app, &which) else { return (StatusCode::NOT_FOUND, "no camera").into_response() };
    match camera::snapshot::capture(&url, "", "", Duration::from_secs(12)).await {
        Ok(jpeg) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], jpeg).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err).into_response(),
    }
}

async fn photo(State(app): State<AppHandle>, Path(name): Path<String>) -> Response {
    let file = {
        let state = app.state::<AppState>();
        let history = state.history.lock().unwrap();
        history.as_ref().and_then(|h| h.photo_path(&name))
    };
    let Some(file) = file else { return StatusCode::NOT_FOUND.into_response() };
    match tokio::fs::read(file).await {
        Ok(jpeg) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], jpeg).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
