//! Falcon Scale Station - the main process, in Rust.
//!
//! One window, one camera, one scale. The scale reader raises a weighing;
//! the pipeline photographs the truck, reads its plate, stamps both on the
//! picture and writes it to the history; the window shows the live picture
//! with the weight and plate over it. The bug report and the updater are
//! still the Electron app's; the window's calls for those answer "not ported
//! yet" (see src/station.js).

mod camera;
mod history;
mod pipeline;
mod plate_choice;
mod plates;
mod scale;
mod server;
mod settings;
mod stamp;

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::mpsc::UnboundedSender;

use history::{History, HistorySettings, Pruned, Stats};

use scale::discovery::{self, Found};
use scale::reader::{self, ScaleEvent, ScaleReader, ScaleSettings, ScaleStatus, TestArgs, TestResult};
use settings::SettingsStore;

const LOG_LINES_KEPT: usize = 500;
// The log also goes to station.log beside the settings file, so what the
// scale did on a day can be read back on the yard PC afterwards. Kept to
// about 2 MB: the older half is dropped when it grows past that.
const LOG_FILE_MAX: u64 = 2 * 1024 * 1024;

pub(crate) struct AppState {
    data_dir: PathBuf,
    server_port: u16,
    pub(crate) settings: Mutex<SettingsStore>,
    pub(crate) scale: Mutex<Option<ScaleReader>>,
    /// None only when the history folder cannot be made.
    pub(crate) history: Mutex<Option<History>>,
    pipeline: UnboundedSender<pipeline::Event>,
    pub(crate) streams: camera::stream::Streams,
    pub(crate) plates: plates::PlateReader,
    log: Mutex<VecDeque<String>>,
}

impl AppState {
    fn log_file(&self) -> PathBuf {
        self.data_dir.join("station.log")
    }

    /// The history folder in use: the chosen one, or the default under app data.
    fn history_folder(&self) -> PathBuf {
        let chosen = self.settings.lock().unwrap().get()["history"]["folder"].as_str().unwrap_or("").trim().to_string();
        if chosen.is_empty() {
            self.data_dir.join("history")
        } else {
            PathBuf::from(chosen)
        }
    }
}

pub(crate) fn log(app: &AppHandle, message: impl AsRef<str>) {
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
            let _ = handle.emit("scale:weighed", &w);
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Weighed(w));
        }
        ScaleEvent::Reweighed(w) => {
            log(&handle, format!("reweighed {} {} (was {})", w.weight, w.unit, w.previous.unwrap_or_default()));
            let _ = handle.emit("scale:weighed", &w);
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Reweighed(w));
        }
        ScaleEvent::Cleared { .. } => {
            log(&handle, "deck clear");
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Cleared);
        }
    });
    *slot = Some(ScaleReader::start(&settings, previous.as_ref(), sink));
    drop(slot);
    log(app, format!("reading the scale at {}:{}", settings.host.trim(), settings.port));
}

// ── History ────────────────────────────────────────────────────────────────

/// Opens the history where the settings say, or points the open one there.
fn start_history(app: &AppHandle) {
    let state = app.state::<AppState>();
    let settings: HistorySettings = state.settings.lock().unwrap().section("history");
    let folder = state.history_folder();
    let mut slot = state.history.lock().unwrap();
    let result = match slot.as_mut() {
        Some(history) => history.configure(folder.clone(), &settings),
        None => {
            let handle = app.clone();
            let on_entry = Box::new(move |entry: &history::Entry| {
                let _ = handle.emit("history:entry", entry);
            });
            History::new(folder.clone(), &settings, on_entry).map(|h| *slot = Some(h))
        }
    };
    drop(slot);
    if let Err(err) = result {
        log(app, format!("history folder {} unusable: {err}", folder.display()));
    }
}

/// Once now and once an hour, for as long as the app runs.
fn prune_history_hourly(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut every_hour = tokio::time::interval(history::PRUNE_INTERVAL);
        loop {
            every_hour.tick().await;
            let pruned = app.state::<AppState>().history.lock().unwrap().as_ref().map(|h| h.prune(reader::now_ms()));
            if let Some(p) = pruned.filter(|p| p.dropped + p.photos_removed > 0) {
                log(&app, format!("history pruned: {} entries, {} photos", p.dropped, p.photos_removed));
            }
        }
    });
}

fn apply_settings(app: &AppHandle) {
    start_history(app);
    start_scale(app);
    // The stream URLs or login may have changed; viewers reconnect to the new ones.
    app.state::<AppState>().streams.stop_all();
    // Load the plate models now rather than when the first truck arrives.
    if app.state::<AppState>().settings.lock().unwrap().get()["plates"]["enabled"] != false {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move { handle.state::<AppState>().plates.warm_up().await });
    }
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
        "serverPort": state.server_port,
        "ffmpeg": camera::ffmpeg::available(),
        "plateReader": state.plates.status(),
        "vehicleModel": false,
        "userData": state.data_dir,
        "settingsFile": state.settings.lock().unwrap().file(),
    })
}

#[tauri::command]
fn app_changelog() -> &'static str {
    include_str!("../../../CHANGELOG.md")
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

/// The Scale tab's "Find the device server": a Moxa search broadcast and a
/// scan of the LAN, with the scan's progress sent as it goes.
#[tauri::command]
async fn scale_discover(app: AppHandle) -> Vec<Found> {
    let found = discovery::discover(|done, total| {
        let _ = app.emit("scale:discover-progress", json!({ "done": done, "total": total }));
    })
    .await;
    log(&app, format!("device server search: {} found", found.len()));
    found
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct DiscoverArgs {
    username: String,
    password: String,
    #[serde(deserialize_with = "scale::frames::lenient_number")]
    onvif_port: f64,
}

/// The Camera tab's "Find cameras": WS-Discovery and a sweep of the LAN,
/// with the sweep's progress sent as it goes.
#[tauri::command]
async fn camera_discover(app: AppHandle, args: Option<DiscoverArgs>) -> camera::discovery::Found {
    let args = args.unwrap_or_default();
    let port = if (1.0..=65535.0).contains(&args.onvif_port) { args.onvif_port as u16 } else { 80 };
    let found = camera::discovery::discover(&args.username, &args.password, port, |done, total, network| {
        let _ = app.emit("camera:discover-progress", json!({ "done": done, "total": total, "network": network }));
    })
    .await;
    log(&app, format!("camera search: {} found on {}", found.cameras.len(), found.networks.join(", ")));
    found
}

/// A camera's streams, asked for with its login.
#[tauri::command]
async fn camera_streams(args: camera::discovery::StreamsArgs) -> Result<Vec<camera::discovery::Stream>, String> {
    camera::discovery::streams_of(args).await
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct SnapshotArgs {
    url: String,
    username: String,
    password: String,
}

/// The Camera tab's "Test live view": one frame, as typed, shown back.
#[tauri::command]
async fn camera_snapshot_test(args: SnapshotArgs) -> Result<Value, String> {
    use base64::Engine;
    let jpeg = camera::snapshot::capture(&args.url, &args.username, &args.password, std::time::Duration::from_secs(12)).await?;
    let (width, height) = camera::snapshot::jpeg_size(&jpeg).unwrap_or((0, 0));
    Ok(json!({
        "width": width,
        "height": height,
        "bytes": jpeg.len(),
        "image": format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&jpeg)),
    }))
}

#[tauri::command]
async fn plates_read_now(app: AppHandle) -> Result<Value, String> {
    pipeline::read_plate_now(&app).await
}

#[tauri::command]
fn plates_status(state: State<AppState>) -> plates::Status {
    state.plates.status()
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct ListArgs {
    limit: Option<usize>,
}

#[tauri::command]
fn history_list(state: State<AppState>, args: Option<ListArgs>) -> Vec<history::Entry> {
    let limit = args.unwrap_or_default().limit.unwrap_or(200);
    state.history.lock().unwrap().as_ref().map(|h| h.list(limit)).unwrap_or_default()
}

#[tauri::command]
fn history_stats(state: State<AppState>) -> Option<Stats> {
    state.history.lock().unwrap().as_ref().map(History::stats)
}

#[tauri::command]
fn history_prune(state: State<AppState>) -> Pruned {
    state.history.lock().unwrap().as_ref().map(|h| h.prune(reader::now_ms())).unwrap_or_default()
}

#[tauri::command]
fn history_open_folder(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    app.opener().open_path(state.history_folder().to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// A folder picker; the chosen path goes back to the History tab, which saves it.
#[tauri::command]
async fn history_choose_folder(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("Where to keep the history log and photos").pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    rx.await.ok().flatten().map(|f| f.to_string())
}

#[tauri::command]
fn log_recent(state: State<AppState>) -> Vec<String> {
    let lines = state.log.lock().unwrap();
    lines.iter().skip(lines.len().saturating_sub(200)).cloned().collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let handle = app.handle().clone();
            let log_handle = handle.clone();
            let logger: camera::stream::Log = std::sync::Arc::new(move |line| log(&log_handle, line));
            let streams = camera::stream::Streams::new(logger.clone());
            let plates = plates::PlateReader::new(plates::Paths::under(&app.path().resource_dir()?), logger);
            app.manage(AppState {
                plates,
                streams,
                settings: Mutex::new(SettingsStore::load(&data_dir)),
                data_dir,
                server_port: server::start(handle.clone())?,
                scale: Mutex::new(None),
                history: Mutex::new(None),
                pipeline: pipeline::start(handle.clone()),
                log: Mutex::new(VecDeque::new()),
            });
            apply_settings(&handle);
            prune_history_hourly(handle.clone());
            log(&handle, format!("station {} started (Rust)", app.package_info().version));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            app_changelog,
            settings_get,
            settings_save,
            settings_defaults,
            scale_status,
            scale_test,
            scale_discover,
            camera_discover,
            camera_streams,
            camera_snapshot_test,
            plates_read_now,
            plates_status,
            history_list,
            history_stats,
            history_prune,
            history_open_folder,
            history_choose_folder,
            log_recent
        ])
        .build(tauri::generate_context!())
        .expect("error while building the station")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<AppState>().streams.stop_all();
            }
        });
}
