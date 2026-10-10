//! Falcon Scale Station - the main process, in Rust.
//!
//! One window, one camera, one scale. So far only the scale is here: the
//! reader raises a weighing and the window shows it. The camera, the plate
//! reader, the history and the updater are still the Electron app's; the
//! window's calls for those answer "not ported yet" (see src/station.js).

mod scale;
mod settings;

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use scale::reader::{self, ScaleEvent, ScaleReader, ScaleSettings, ScaleStatus, TestArgs, TestResult};
use settings::SettingsStore;

const LOG_LINES_KEPT: usize = 500;
// The log also goes to station.log beside the settings file, so what the
// scale did on a day can be read back on the yard PC afterwards. Kept to
// about 2 MB: the older half is dropped when it grows past that.
const LOG_FILE_MAX: u64 = 2 * 1024 * 1024;

struct AppState {
    data_dir: PathBuf,
    settings: Mutex<SettingsStore>,
    scale: Mutex<Option<ScaleReader>>,
    log: Mutex<VecDeque<String>>,
}

impl AppState {
    fn log_file(&self) -> PathBuf {
        self.data_dir.join("station.log")
    }
}

fn log(app: &AppHandle, message: impl AsRef<str>) {
    let state = app.state::<AppState>();
    let line = format!("{} {}", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"), message.as_ref());
    println!("{line}");
    {
        let mut lines = state.log.lock().unwrap();
        lines.push_back(line.clone());
        if lines.len() > LOG_LINES_KEPT {
            lines.pop_front();
        }
    }
    append_log_file(&state.log_file(), &line);
    let _ = app.emit("log", &line);
}

fn append_log_file(file: &std::path::Path, line: &str) {
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(file) else { return };
    let _ = writeln!(f, "{line}");
    if f.metadata().map(|m| m.len() > LOG_FILE_MAX).unwrap_or(false) {
        if let Ok(text) = fs::read_to_string(file) {
            let half = text.len() / 2;
            let cut = text[half..].find('\n').map_or(half, |i| half + i + 1);
            let _ = fs::write(file, &text[cut..]);
        }
    }
}

// ── Scale ──────────────────────────────────────────────────────────────────

/// (Re)starts the reader from the saved settings, carrying over a truck that
/// is on the deck right now.
fn start_scale(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settings: ScaleSettings = state.settings.lock().unwrap().section("scale");
    let mut slot = state.scale.lock().unwrap();
    let previous = slot.take();
    if let Some(previous) = &previous {
        previous.stop();
    }
    if !settings.enabled || settings.host.trim().is_empty() {
        let _ = app.emit("scale:status", Value::Null);
        return;
    }
    let handle = app.clone();
    let sink = std::sync::Arc::new(move |event: ScaleEvent| match event {
        ScaleEvent::Status(status) => {
            let _ = handle.emit("scale:status", status);
        }
        ScaleEvent::Weighed(w) => {
            log(&handle, format!("weighed {} {}", w.weight, w.unit));
            let _ = handle.emit("scale:weighed", w);
            // Not ported yet: the weighing pipeline (photo, plate, history).
        }
        ScaleEvent::Reweighed(w) => {
            log(&handle, format!("reweighed {} {} (was {})", w.weight, w.unit, w.previous.unwrap_or_default()));
            let _ = handle.emit("scale:weighed", w);
        }
        ScaleEvent::Cleared { .. } => log(&handle, "deck clear"),
    });
    *slot = Some(ScaleReader::start(&settings, previous.as_ref(), sink));
    drop(slot);
    log(app, format!("reading the scale at {}:{}", settings.host.trim(), settings.port));
}

fn apply_settings(app: &AppHandle) {
    start_scale(app);
    let window = app.state::<AppState>().settings.lock().unwrap().get()["window"].clone();
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_always_on_top(window["alwaysOnTop"].as_bool().unwrap_or(false));
        let fullscreen = window["fullscreen"].as_bool().unwrap_or(false);
        if win.is_fullscreen().ok() != Some(fullscreen) {
            let _ = win.set_fullscreen(fullscreen);
        }
    }
}

// ── Commands: what the window can call (src/station.js) ─────────────────────

#[tauri::command]
fn app_info(app: AppHandle, state: State<AppState>) -> Value {
    json!({
        "version": app.package_info().version.to_string(),
        "serverPort": 0,
        "ffmpeg": false,
        "plateReader": { "installed": false, "ready": false, "lastError": "not ported to Rust yet" },
        "vehicleModel": false,
        "userData": state.data_dir,
        "settingsFile": state.settings.lock().unwrap().file(),
    })
}

#[tauri::command]
fn app_changelog() -> &'static str {
    include_str!("../../CHANGELOG.md")
}

#[tauri::command]
fn settings_get(state: State<AppState>) -> Value {
    state.settings.lock().unwrap().get().clone()
}

#[tauri::command]
fn settings_save(app: AppHandle, state: State<AppState>, patch: Value) -> Result<Value, String> {
    let next = state.settings.lock().unwrap().save(&patch).map_err(|e| e.to_string())?.clone();
    apply_settings(&app);
    Ok(next)
}

#[tauri::command]
fn settings_defaults() -> Value {
    settings::defaults()
}

#[tauri::command]
fn scale_status(state: State<AppState>) -> Option<ScaleStatus> {
    state.scale.lock().unwrap().as_ref().map(ScaleReader::status)
}

#[tauri::command]
async fn scale_test(args: TestArgs) -> TestResult {
    reader::test_connection(args).await
}

#[tauri::command]
fn log_recent(state: State<AppState>) -> Vec<String> {
    let lines = state.log.lock().unwrap();
    lines.iter().skip(lines.len().saturating_sub(200)).cloned().collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            app.manage(AppState { settings: Mutex::new(SettingsStore::load(&data_dir)), data_dir, scale: Mutex::new(None), log: Mutex::new(VecDeque::new()) });
            let handle = app.handle().clone();
            apply_settings(&handle);
            log(&handle, format!("station {} started (Rust)", app.package_info().version));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![app_info, app_changelog, settings_get, settings_save, settings_defaults, scale_status, scale_test, log_recent])
        .run(tauri::generate_context!())
        .expect("error while running the station");
}
