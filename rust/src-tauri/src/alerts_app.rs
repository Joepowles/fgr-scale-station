//! The alerts in the running app: what feeds alerts.rs, and what is done
//! with what it decides - the window plays the sound and shows the banner
//! (the "alert" event), Windows shows a notification, the log keeps a line.
//! Also the tray icon, with "Mute alerts for an hour", and the camera motion
//! watcher, run while an alert needs it.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::alerts::{self, AlertSettings, Engine, Fired, Input, Rule};
use crate::camera::{self, motion};
use crate::scale::reader;
use crate::{log, AppState};

const TRAY_ID: &str = "main";
const TICK: Duration = Duration::from_millis(500);

pub struct Alerts {
    engine: Mutex<Engine>,
    watcher: Mutex<Option<(String, motion::Watcher)>>,
    started_at: i64,
}

impl Alerts {
    pub fn new() -> Self {
        Self { engine: Mutex::default(), watcher: Mutex::default(), started_at: reader::now_ms() }
    }
}

fn settings(app: &AppHandle) -> AlertSettings {
    app.state::<AppState>().settings.lock().unwrap().section("alerts")
}

fn quiet_now(s: &AlertSettings) -> bool {
    let now = Local::now();
    alerts::in_quiet_hours(&s.quiet, now.weekday().num_days_from_sunday() as u8, now.hour() * 60 + now.minute())
}

/// Something happened: alert as the rules say.
pub fn feed(app: &AppHandle, input: Input) {
    let state = app.state::<AppState>();
    let s = settings(app);
    let fired = state.alerts.engine.lock().unwrap().handle(&s, &input, reader::now_ms(), quiet_now(&s));
    for f in fired {
        give(app, &f);
    }
}

fn give(app: &AppHandle, f: &Fired) {
    let silenced = if f.silenced.is_empty() { String::new() } else { format!(" (silent: {})", f.silenced) };
    log(app, format!("alert: {} - {}{silenced}", f.name, f.message));
    let _ = app.emit("alert", f);
    if f.notify {
        let shown = app.notification().builder().title(if f.name.is_empty() { "Falcon Scale Station" } else { &f.name }).body(&f.message).show();
        if let Err(err) = shown {
            log(app, format!("could not show a notification: {err}"));
        }
    }
}

/// The scale's weight and readings, every half second, for the rules that
/// watch them over time.
pub fn start_ticking(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut every = tokio::time::interval(TICK);
        loop {
            every.tick().await;
            let state = app.state::<AppState>();
            let status = state.scale.lock().unwrap().as_ref().map(|s| s.status());
            let started_at = state.alerts.started_at;
            let input = Input::Tick {
                weight: status.as_ref().and_then(|s| s.weight),
                scale_enabled: status.is_some(),
                last_reading_at: status.as_ref().and_then(|s| s.last_frame_at),
                started_at,
            };
            feed(&app, input);
        }
    });
}

/// After the settings are saved: the motion watcher runs if an alert needs
/// it, and the tray icon shows if wanted.
pub fn apply(app: &AppHandle) {
    let s = settings(app);
    let state = app.state::<AppState>();
    let wants_camera = s.enabled && s.rules.iter().any(|r| r.enabled && (r.kind == alerts::kind::MOTION || r.kind == alerts::kind::CAMERA_OFFLINE));
    let camera: camera::CameraSettings = state.settings.lock().unwrap().section("camera");
    let url = camera.url(false).filter(|_| wants_camera);
    let mut watcher = state.alerts.watcher.lock().unwrap();
    if watcher.as_ref().map(|(u, _)| u) != url.as_ref() {
        *watcher = url.map(|url| {
            let handle = app.clone();
            let w = motion::Watcher::start(url.clone(), move |changed| {
                feed(&handle, Input::CameraFrame);
                feed(&handle, Input::Motion { changed });
            });
            (url, w)
        });
        let since = watcher.is_some().then(reader::now_ms);
        state.alerts.engine.lock().unwrap().watching_camera(since);
        log(app, if since.is_some() { "alerts: watching the camera picture" } else { "alerts: not watching the camera picture" });
    }
    drop(watcher);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(s.tray_icon);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Mutes alert sounds and notifications until `until` (None: unmuted), and
/// says so to the window and the tray.
pub fn mute(app: &AppHandle, until: Option<i64>) -> Value {
    let state = app.state::<AppState>();
    state.alerts.engine.lock().unwrap().mute(until);
    let status = status(app);
    let _ = app.emit("alerts:status", &status);
    if let Some(item) = app.try_state::<TrayMute>() {
        let _ = item.0.set_text(if until.is_some() { "Unmute alerts" } else { "Mute alerts for an hour" });
    }
    log(
        app,
        match until {
            Some(t) => format!(
                "alerts muted until {}",
                chrono::DateTime::from_timestamp_millis(t).map(|d| d.with_timezone(&Local).format("%-I:%M %p").to_string()).unwrap_or_default()
            ),
            None => "alerts unmuted".into(),
        },
    );
    status
}

pub fn status(app: &AppHandle) -> Value {
    let muted_until = app.state::<AppState>().alerts.engine.lock().unwrap().muted_until(reader::now_ms());
    json!({ "mutedUntil": muted_until })
}

/// The Test button: the rule as it would go off now.
pub fn test(app: &AppHandle, rule: &Rule) {
    let state = app.state::<AppState>();
    let s = settings(app);
    let f = state.alerts.engine.lock().unwrap().test(&s, rule, reader::now_ms(), quiet_now(&s));
    give(app, &f);
}

/// The tray menu's mute item, kept to relabel it.
struct TrayMute(MenuItem<tauri::Wry>);

/// The tray icon: click it for the window; its menu shows the window, mutes
/// alerts for an hour, or quits.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Falcon Scale Station", true, None::<&str>)?;
    let mute_item = MenuItem::with_id(app, "mute", "Mute alerts for an hour", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &mute_item, &quit])?;
    app.manage(TrayMute(mute_item));
    let mut tray = TrayIconBuilder::with_id(TRAY_ID).tooltip("Falcon Scale Station").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.on_menu_event(|app, event| match event.id.as_ref() {
        "show" => show_main(app),
        "mute" => {
            let muted = app.state::<AppState>().alerts.engine.lock().unwrap().muted_until(reader::now_ms()).is_some();
            mute(app, (!muted).then(|| reader::now_ms() + 60 * 60 * 1000));
        }
        "quit" => app.exit(0),
        _ => {}
    })
    .on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            show_main(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}
