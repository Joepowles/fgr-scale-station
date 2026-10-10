//! The local HTTP server the window gets pictures from, because <img> and
//! <video> want a URL, not a buffer:
//!
//!   http://127.0.0.1:<port>/photo/<name>  a history photo
//!
//! Bound to 127.0.0.1 on a port the OS picks, so nothing off this PC can
//! reach it. The camera's /stream and /snapshot join it when the camera is
//! ported.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tauri::{AppHandle, Manager};

use crate::AppState;

/// Binds now, so the port is known before the window asks for it, and
/// serves in the background.
pub fn start(app: AppHandle) -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let router = Router::new().route("/photo/{name}", get(photo)).with_state(app);
    tauri::async_runtime::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("listener from std");
        let _ = axum::serve(listener, router).await;
    });
    Ok(port)
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
