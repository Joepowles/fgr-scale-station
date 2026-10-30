//! Falcon Scale Station - the main process, in Rust.
//!
//! One window, one camera, one scale. The scale reader raises a weighing;
//! the pipeline photographs the truck, reads its plate, stamps both on the
//! picture and writes it to the history; the window shows the live picture
//! with the weight and plate over it.

mod alerts;
mod alerts_app;
mod bug_report;
mod camera;
mod crash;
mod electron_import;
mod history;
mod pairing;
mod pipeline;
mod plate_choice;
mod plates;
mod samples;
mod scale;
mod server;
mod settings;
mod stamp;
mod truck_photos;
mod trucks;
mod updater;
mod window_state;

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tokio::sync::mpsc::UnboundedSender;

use history::{History, HistorySettings, Pruned, Stats};

use scale::discovery::{self, Found};
use scale::reader::{self, ScaleEvent, ScaleReader, ScaleSettings, ScaleStatus, TestArgs, TestResult};
use settings::SettingsStore;

const LOG_LINES_KEPT: usize = 500;
const RELEASES_PAGE: &str = "https://github.com/Joepowles/fgr-scale-station/releases";
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
    updater: std::sync::Arc<updater::Updater>,
    pub(crate) alerts: alerts_app::Alerts,
    pub(crate) samples: samples::Samples,
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
        // Not a word to the reader in here: this runs under its lock, and
        // asking it for its status froze the scale reading (0.2.2, 0.2.3).
        ScaleEvent::Unsettled { seconds, min, max, motion, tolerance, unit } => {
            log(
                &handle,
                format!(
                    "truck on the scale {seconds} s, not settled: {} to {} in the last readings ({} apart, settles within {}){}",
                    scale::frames::format_weight(min, unit),
                    scale::frames::format_weight(max, unit),
                    scale::frames::format_weight(max - min, unit),
                    scale::frames::format_weight(tolerance, unit),
                    if motion { "; the scale's motion flag is on" } else { "" }
                ),
            );
        }
        ScaleEvent::Arrived => {
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Arrived);
            samples::on_arrived(&handle);
            alerts_app::feed(&handle, alerts::Input::Arrived);
        }
        ScaleEvent::Weighed(w) => {
            let follows = w.follows.map_or(String::new(), |f| {
                format!(" - the next truck: it pulled on before the one weighed at {} had left", scale::frames::format_weight(f, w.unit))
            });
            log(
                &handle,
                format!(
                    "weighed {} {}{}{follows}",
                    w.weight,
                    w.unit,
                    if w.estimated { " (not settled: steady enough after the wait, so weighed anyway)" } else { "" }
                ),
            );
            let _ = handle.emit("scale:weighed", &w);
            alerts_app::feed(&handle, alerts::Input::Weighed { weight: w.weight, unit: w.unit.to_string() });
            // A truck can settle without an arrival seen first.
            samples::on_arrived(&handle);
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Weighed(w));
        }
        ScaleEvent::Reweighed(w) => {
            log(&handle, format!("reweighed {} {} (was {})", w.weight, w.unit, w.previous.unwrap_or_default()));
            let _ = handle.emit("scale:weighed", &w);
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Reweighed(w));
        }
        ScaleEvent::Moving { weight, locked, unit } => {
            log(
                &handle,
                format!(
                    "the truck weighed at {} is moving: {} now; nothing more goes on its entry until the scale settles again",
                    scale::frames::format_weight(locked, unit),
                    scale::frames::format_weight(weight, unit)
                ),
            );
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Moving);
        }
        ScaleEvent::Stopped { weight, locked, same, unit } => {
            let (w, l) = (scale::frames::format_weight(weight, unit), scale::frames::format_weight(locked, unit));
            log(
                &handle,
                if same {
                    format!("settled again at {w}, back at the {l} it was weighed at: the same truck")
                } else {
                    format!("settled at {w}, lighter than the {l} weighed and nothing came on since: part of the truck still on as it pulls off")
                },
            );
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Stopped { weight, same });
        }
        ScaleEvent::Note(message) => log(&handle, message),
        ScaleEvent::Cleared { .. } => {
            log(&handle, "deck clear");
            let _ = handle.state::<AppState>().pipeline.send(pipeline::Event::Cleared);
            samples::on_cleared(&handle);
            alerts_app::feed(&handle, alerts::Input::Cleared);
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
    learn_hd_shape(app);
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
    app.state::<AppState>().updater.settings_changed();
    alerts_app::apply(app);
    // Start with Windows: a Run entry for this user, as Electron's login item.
    let start_with_windows = window["startWithWindows"].as_bool().unwrap_or(false);
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().ok() != Some(start_with_windows) {
        let result = if start_with_windows { autolaunch.enable() } else { autolaunch.disable() };
        if let Err(err) = result {
            log(app, format!("could not change starting with Windows: {err}"));
        }
    }
}

/// One frame from the full-resolution stream, for its picture size: the live
/// view shows the small stream in that shape (issue #13); the window is told
/// to open the stream again, which it does without the picture going away.
fn learn_hd_shape(app: &AppHandle) {
    let camera: camera::CameraSettings = app.state::<AppState>().settings.lock().unwrap().section("camera");
    let (Some(sub), Some(hd)) = (camera.url(false), camera.url(true)) else { return };
    if sub == hd {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match camera::snapshot::capture(&hd, "", "", std::time::Duration::from_secs(15)).await {
            Ok(jpeg) => {
                let Some((w, h)) = camera::snapshot::jpeg_size(&jpeg) else { return };
                let state = app.state::<AppState>();
                if state.streams.note_shape(&hd, w.into(), h.into()) {
                    log(&app, format!("live view: the small stream is shown at the full-resolution stream's shape, {w}x{h}"));
                    let _ = app.emit("camera:shape", ());
                }
            }
            Err(err) => log(&app, format!("live view: could not read the full-resolution stream's size ({err})")),
        }
    });
}

// ── Commands: what the window can call (src/station.js) ─────────────────────

#[tauri::command]
fn app_info(app: AppHandle, state: State<AppState>) -> Value {
    info(&app, &state)
}

fn info(app: &AppHandle, state: &AppState) -> Value {
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

/// Async, so the window never waits on the scale reader: a stuck reader froze
/// the whole window when it asked (0.2.3, Test connection).
#[tauri::command]
async fn scale_status(state: State<'_, AppState>) -> Result<Option<ScaleStatus>, String> {
    Ok(scale_status_now(&state))
}

fn scale_status_now(state: &AppState) -> Option<ScaleStatus> {
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

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct NameArgs {
    id: String,
    name: String,
}

/// The operator names the truck of a weighing ("Hoffman/Steve"). The name
/// goes with its plate from then on, and can pair a truck with no plate.
#[tauri::command]
fn history_name(app: AppHandle, state: State<AppState>, args: Option<NameArgs>) -> Result<Option<history::Entry>, String> {
    let args = args.unwrap_or_default();
    let mut guard = state.history.lock().unwrap();
    let Some(history) = guard.as_mut() else { return Ok(None) };
    let Some(entry) = history.name(&args.id, &args.name).map_err(|e| e.to_string())? else { return Ok(None) };
    let named = trucks::name_of(&entry).filter(|_| trucks::given(&entry));
    log(&app, named.map_or_else(|| format!("weighing {}: name taken off", args.id), |n| format!("weighing {}: named {n}", args.id)));
    match pairing::update(history, &args.id) {
        Ok(Some(line)) => log(&app, format!("weighing {}: {line}", args.id)),
        Ok(None) => {}
        Err(err) => log(&app, format!("could not pair the weighing: {err}")),
    }
    Ok(history.list(usize::MAX).into_iter().find(|e| e.get("id").and_then(Value::as_str) == Some(args.id.as_str())))
}

/// The names given so far, latest first, to offer as the operator types.
#[tauri::command]
fn history_names(state: State<AppState>) -> Vec<String> {
    state.history.lock().unwrap().as_ref().map(|h| h.names().to_vec()).unwrap_or_default()
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

/// The Reconnect button over the live view.
#[tauri::command]
fn camera_reconnect(state: State<AppState>) -> bool {
    state.streams.reconnect();
    true
}

/// The window's own reports, such as what the live view is showing.
#[tauri::command]
fn log_write(app: AppHandle, args: Option<String>) -> bool {
    log(&app, args.unwrap_or_default().chars().take(500).collect::<String>());
    true
}

#[tauri::command]
fn app_copy(app: AppHandle, args: Option<String>) -> Result<bool, String> {
    app.clipboard().write_text(args.unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
fn app_open_releases(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(RELEASES_PAGE, None::<&str>).map_err(|e| e.to_string())
}

// ── The Bug report tab ─────────────────────────────────────────────────────
// The report is what was typed plus the app's own account of itself: version,
// settings without passwords, the last weighings, the tail of the log. Filed
// on GitHub, or saved to a file to send some other way when the PC has no
// internet.

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct ReportArgs {
    description: String,
    /// "idea" for an idea; anything else is a problem.
    kind: String,
    repo: String,
    token: String,
}

fn build_bug_report(app: &AppHandle, description: &str, idea: bool) -> bug_report::Report {
    let state = app.state::<AppState>();
    let log = fs::read_to_string(state.log_file()).unwrap_or_else(|_| state.log.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n"));
    let history: Vec<Value> =
        state.history.lock().unwrap().as_ref().map(|h| h.list(bug_report::HISTORY_ENTRIES)).unwrap_or_default().into_iter().map(Value::Object).collect();
    let scale = state.scale.lock().unwrap().as_ref().map(|s| serde_json::to_value(s.status()).unwrap_or_default());
    let settings = state.settings.lock().unwrap().get().clone();
    bug_report::build(&bug_report::Input {
        description,
        info: &info(app, &state),
        settings: &settings,
        log: &log,
        history: &history,
        scale: scale.as_ref(),
        machine: bug_report::machine(),
        idea,
        now: chrono::Utc::now(),
    })
}

/// A report for crash.rs: built from whatever can be had without waiting,
/// since part of the app may be stuck holding a lock.
pub(crate) fn careful_report(app: &AppHandle, description: &str) -> bug_report::Report {
    let state = app.state::<AppState>();
    let log = fs::read_to_string(state.log_file()).unwrap_or_default();
    let history: Vec<Value> = state
        .history
        .try_lock()
        .ok()
        .and_then(|h| h.as_ref().map(|h| h.list(bug_report::HISTORY_ENTRIES)))
        .unwrap_or_default()
        .into_iter()
        .map(Value::Object)
        .collect();
    let scale = state.scale.try_lock().ok().and_then(|s| s.as_ref().and_then(|s| s.try_status())).map(|s| serde_json::to_value(s).unwrap_or_default());
    let settings = state.settings.try_lock().map(|s| s.get().clone()).unwrap_or_default();
    let info = json!({
        "version": app.package_info().version.to_string(),
        "ffmpeg": camera::ffmpeg::available(),
        "plateReader": { "installed": state.plates.installed() },
    });
    bug_report::build(&bug_report::Input {
        description,
        info: &info,
        settings: &settings,
        log: &log,
        history: &history,
        scale: scale.as_ref(),
        machine: bug_report::machine(),
        idea: false,
        now: chrono::Utc::now(),
    })
}

#[tauri::command]
async fn support_send(app: AppHandle, args: Option<ReportArgs>) -> Result<bug_report::Issue, String> {
    let args = args.unwrap_or_default();
    let report = build_bug_report(&app, &args.description, args.kind == "idea");
    let mut support = app.state::<AppState>().settings.lock().unwrap().get()["support"].clone();
    if !args.repo.trim().is_empty() {
        support["repo"] = args.repo.into();
    }
    if !args.token.trim().is_empty() {
        support["githubToken"] = args.token.into();
    }
    let builtin_relay = settings::defaults()["support"]["relayUrl"].as_str().unwrap_or("").to_string();
    let issue = bug_report::send(&*bug_report::http_post(), &support, &builtin_relay, &report).await?;
    log(&app, format!("bug report filed: {}", issue.url.as_deref().unwrap_or("?")));
    Ok(issue)
}

#[tauri::command]
async fn support_save(app: AppHandle, args: Option<ReportArgs>) -> Result<Option<String>, String> {
    let args = args.unwrap_or_default();
    let report = build_bug_report(&app, &args.description, args.kind == "idea");
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H-%M-%S");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog =
        app.dialog().file().set_title("Save the bug report").set_file_name(format!("scale-station-report-{stamp}.md")).add_filter("Markdown", &["md"]);
    if let Ok(desktop) = app.path().desktop_dir() {
        dialog = dialog.set_directory(desktop);
    }
    dialog.save_file(move |file| {
        let _ = tx.send(file);
    });
    let Some(file) = rx.await.ok().flatten() else { return Ok(None) };
    let path = file.into_path().map_err(|e| e.to_string())?;
    fs::write(&path, format!("# {}\n\n{}", report.title, report.body)).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// The link to a filed report; only GitHub's.
#[tauri::command]
fn support_open(app: AppHandle, args: Option<String>) -> Result<(), String> {
    let url = args.unwrap_or_default();
    if !url.starts_with("https://github.com/") {
        return Ok(());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ── The history window ─────────────────────────────────────────────────────
// The history in a window of its own (issue #12), to sit beside the camera
// picture or on another screen rather than over it. The same window as the
// main one, opened at #history, which shows only the history.

const HISTORY_WINDOW: &str = "history";
/// There while the history window is open, so it opens again with the app.
const HISTORY_OPEN_FILE: &str = "history-window-open";
/// The app is closing: the history window going too is not the user closing it.
static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn open_history_window(app: &AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(HISTORY_WINDOW) {
        let _ = win.unminimize();
        let _ = win.show();
        return win.set_focus().map_err(|e| e.to_string());
    }
    let dir = app.state::<AppState>().data_dir.clone();
    let win = tauri::WebviewWindowBuilder::new(app, HISTORY_WINDOW, tauri::WebviewUrl::App("index.html#history".into()))
        .title("Falcon Scale Station - History")
        .inner_size(560.0, 820.0)
        .min_inner_size(360.0, 300.0)
        .background_color(tauri::window::Color(0, 0, 0, 255))
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;
    window_state::restore(&win, &dir, window_state::HISTORY_FILE);
    window_state::track(&win, &dir, window_state::HISTORY_FILE);
    let _ = fs::write(dir.join(HISTORY_OPEN_FILE), "");
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { .. } = event {
            if !QUITTING.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = fs::remove_file(dir.join(HISTORY_OPEN_FILE));
            }
        }
    });
    Ok(())
}

/// Async, and it must stay so: a window built in a synchronous command
/// deadlocks the main thread on Windows (WebView2), and the whole app froze -
/// every call from the window waiting for ever, the history showing empty.
#[tauri::command]
async fn window_open_history(app: AppHandle) -> Result<(), String> {
    open_history_window(&app)
}

// ── Alerts ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn alerts_test(app: AppHandle, args: alerts::Rule) {
    alerts_app::test(&app, &args);
}

/// Mutes alerts for `args` minutes; 0 or none unmutes.
#[tauri::command]
fn alerts_mute(app: AppHandle, args: Option<f64>) -> Value {
    let minutes = args.unwrap_or(0.0);
    alerts_app::mute(&app, (minutes > 0.0).then(|| reader::now_ms() + (minutes * 60_000.0) as i64))
}

#[tauri::command]
fn alerts_status(app: AppHandle) -> Value {
    alerts_app::status(&app)
}

/// A sound file for an alert: WAV, MP3 or OGG.
#[tauri::command]
async fn alerts_choose_sound(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_title("A sound for this alert").add_filter("Sound", &["wav", "mp3", "ogg", "m4a"]).pick_file(move |file| {
        let _ = tx.send(file);
    });
    rx.await.ok().flatten().and_then(|f| f.into_path().ok()).map(|p| p.display().to_string())
}

// ── Updates ────────────────────────────────────────────────────────────────

#[tauri::command]
async fn update_check(state: State<'_, AppState>) -> Result<Value, String> {
    Ok(state.updater.check_now().await)
}

#[tauri::command]
fn update_status(state: State<AppState>) -> Option<Value> {
    state.updater.status()
}

/// Lets go of everything an update replaces: ffmpeg and the plate reader
/// run from the install folder.
fn release_install_folder(app: &AppHandle) {
    let state = app.state::<AppState>();
    state.streams.stop_all();
    tauri::async_runtime::block_on(state.plates.stop());
}

/// The Updates tab's "Restart to update": closes the app, installs the
/// downloaded update and opens it again.
#[tauri::command]
fn update_install(app: AppHandle) -> bool {
    release_install_folder(&app);
    if !app.state::<AppState>().updater.install(true) {
        return false;
    }
    app.exit(0);
    true
}

#[tauri::command]
fn log_recent(state: State<AppState>) -> Vec<String> {
    let lines = state.log.lock().unwrap();
    lines.iter().skip(lines.len().saturating_sub(200)).cloned().collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A copy the watchdog restarted waits for the frozen one to be gone.
    crash::wait_if_restarted();
    crash::install_panic_hook();
    tauri::Builder::default()
        // One copy at a time, registered first as the plugin asks. A second
        // copy reads the same scale, so every truck was weighed and logged
        // twice (issue #5); opening the app again brings the window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            let data_dir_for_window = data_dir.clone();
            // The first start on a PC that had the Electron app: carry on from its settings.
            let electron_dir = electron_import::electron_data_dir(&app.path().config_dir()?);
            let imported = electron_import::import(&electron_dir, &data_dir);
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
                updater: updater::Updater::new(&handle, app.path().app_local_data_dir()?.join("updates")),
                alerts: alerts_app::Alerts::new(),
                samples: samples::Samples::new(&data_dir_for_window),
                log: Mutex::new(VecDeque::new()),
            });
            samples::start_uploader(handle.clone());
            if let Some(win) = app.get_webview_window("main") {
                window_state::restore(&win, &data_dir_for_window, window_state::FILE);
                window_state::track(&win, &data_dir_for_window, window_state::FILE);
                // Closing the main window closes the app, the history window with it.
                let main_closing = handle.clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { .. } = event {
                        QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
                        if let Some(history) = main_closing.get_webview_window(HISTORY_WINDOW) {
                            let _ = history.close();
                        }
                    }
                });
                // The header's full-screen button follows F11 and the like.
                let (events, handle) = (win.clone(), handle.clone());
                let full = std::sync::atomic::AtomicBool::new(win.is_fullscreen().unwrap_or(false));
                win.on_window_event(move |event| {
                    if let WindowEvent::Resized(_) = event {
                        let now = events.is_fullscreen().unwrap_or(false);
                        if full.swap(now, std::sync::atomic::Ordering::SeqCst) != now {
                            let _ = handle.emit("window:fullscreen", now);
                        }
                    }
                });
            }
            match imported {
                Ok(Some(i)) => log(
                    &handle,
                    format!(
                        "settings brought over from the Electron app ({}){}",
                        i.from.display(),
                        i.history_folder.map_or(String::new(), |f| format!("; history kept in {f}"))
                    ),
                ),
                Ok(None) => {}
                Err(err) => log(&handle, format!("could not bring the Electron app's settings over: {err}")),
            }
            crash::start(&handle, &data_dir_for_window);
            if let Err(err) = alerts_app::build_tray(&handle) {
                log(&handle, format!("could not make the tray icon: {err}"));
            }
            apply_settings(&handle);
            alerts_app::start_ticking(handle.clone());
            // Open again if it was open when the app last closed.
            if data_dir_for_window.join(HISTORY_OPEN_FILE).exists() {
                if let Err(err) = open_history_window(&handle) {
                    log(&handle, format!("could not open the history window: {err}"));
                }
            }
            handle.state::<AppState>().updater.start();
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
            camera_reconnect,
            plates_read_now,
            plates_status,
            history_list,
            history_name,
            history_names,
            history_stats,
            history_prune,
            history_open_folder,
            history_choose_folder,
            log_recent,
            log_write,
            app_copy,
            app_open_releases,
            support_send,
            support_save,
            support_open,
            update_check,
            update_status,
            update_install,
            window_open_history,
            alerts_test,
            alerts_mute,
            alerts_status,
            alerts_choose_sound
        ])
        .build(tauri::generate_context!())
        .expect("error while building the station")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // A downloaded update goes in as the app closes, as the
                // Electron app's did; it is opened again next time as usual.
                release_install_folder(app);
                app.state::<AppState>().updater.install(false);
                crash::clean_exit(&app.state::<AppState>().data_dir);
            }
        });
}
