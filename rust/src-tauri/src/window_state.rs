//! Where the window was last: its size and position, and whether it was
//! maximised, kept in window-state.json beside the settings so the app comes
//! back where it was left. A position on a monitor that is no longer there (a
//! second screen unplugged, a different resolution) is dropped and the window
//! opens at its default size on the main screen. The Electron app's
//! windowState.js, with the same file.
//!
//! Sizes and positions are kept in logical pixels (Electron's DIPs), so a
//! file imported from the Electron app means the same place.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{LogicalPosition, LogicalSize, WebviewWindow, WindowEvent};

/// The main window's; the history window has its own (HISTORY_FILE).
pub const FILE: &str = "window-state.json";
pub const HISTORY_FILE: &str = "history-window-state.json";
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Logical px of the window that must sit on some screen.
const MIN_VISIBLE: f64 = 100.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub maximized: bool,
}

/// A screen's work area, in logical pixels.
#[derive(Debug, Clone, Copy)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn read(dir: &Path, file: &str) -> Option<Saved> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(file)).ok()?).ok()
}

pub fn write(dir: &Path, file: &str, saved: &Saved) {
    if let Ok(text) = serde_json::to_string_pretty(saved) {
        let _ = std::fs::write(dir.join(file), text);
    }
}

/// The bounds to open with, or None when the saved ones would land off
/// every screen.
pub fn placement_for(saved: Option<Saved>, screens: &[Area]) -> Option<Saved> {
    let s = saved?;
    if ![s.x, s.y, s.width, s.height].iter().all(|v| v.is_finite()) || s.width < 200.0 || s.height < 150.0 {
        return None;
    }
    let visible = screens.iter().any(|a| {
        let w = (s.x + s.width).min(a.x + a.width) - s.x.max(a.x);
        let h = (s.y + s.height).min(a.y + a.height) - s.y.max(a.y);
        w >= MIN_VISIBLE && h >= MIN_VISIBLE
    });
    visible.then_some(s)
}

fn screens(win: &WebviewWindow) -> Vec<Area> {
    win.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let (area, scale) = (m.work_area(), m.scale_factor());
            Area {
                x: f64::from(area.position.x) / scale,
                y: f64::from(area.position.y) / scale,
                width: f64::from(area.size.width) / scale,
                height: f64::from(area.size.height) / scale,
            }
        })
        .collect()
}

/// Puts the window back where it was, then shows it. The window starts
/// hidden (tauri.conf.json) so it is never seen jumping there.
pub fn restore(win: &WebviewWindow, dir: &Path, file: &str) {
    if let Some(p) = placement_for(read(dir, file), &screens(win)) {
        let _ = win.set_size(LogicalSize::new(p.width, p.height));
        let _ = win.set_position(LogicalPosition::new(p.x, p.y));
        if p.maximized {
            let _ = win.maximize();
        }
    }
    let _ = win.show();
}

/// What to keep of the window now. Fullscreen and maximised sizes are the
/// screen's, not the user's, so the last normal bounds are what is kept,
/// with the maximised flag beside them.
fn capture(win: &WebviewWindow, state: &mut Saved) {
    let full = win.is_fullscreen().unwrap_or(false);
    let maximized = win.is_maximized().unwrap_or(false);
    let minimized = win.is_minimized().unwrap_or(false);
    if !full && !maximized && !minimized {
        let scale = win.scale_factor().unwrap_or(1.0);
        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
            let (pos, size) = (pos.to_logical::<f64>(scale), size.to_logical::<f64>(scale));
            *state = Saved { x: pos.x, y: pos.y, width: size.width, height: size.height, maximized: state.maximized };
        }
    }
    if !full {
        state.maximized = maximized;
    }
}

/// Saves the window's placement as it changes, and once more as it closes.
pub fn track(win: &WebviewWindow, dir: &Path, file: &'static str) {
    let state = Arc::new(Mutex::new(read(dir, file).unwrap_or_default()));
    let generation = Arc::new(AtomicU64::new(0));
    let (window, dir) = (win.clone(), dir.to_path_buf());
    win.on_window_event(move |event| match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            // Saved once things have been still for a moment.
            let this = generation.fetch_add(1, Ordering::SeqCst) + 1;
            let (generation, window, dir, state) = (generation.clone(), window.clone(), dir.clone(), state.clone());
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(SAVE_DELAY).await;
                if generation.load(Ordering::SeqCst) == this {
                    let mut s = state.lock().unwrap();
                    capture(&window, &mut s);
                    write(&dir, file, &s);
                }
            });
        }
        WindowEvent::CloseRequested { .. } => {
            generation.fetch_add(1, Ordering::SeqCst);
            let mut s = state.lock().unwrap();
            capture(&window, &mut s);
            write(&dir, file, &s);
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Area = Area { x: 0.0, y: 0.0, width: 1920.0, height: 1040.0 };
    const SECOND: Area = Area { x: 1920.0, y: 0.0, width: 1920.0, height: 1080.0 };
    fn at(x: f64, y: f64, width: f64, height: f64) -> Option<Saved> {
        Some(Saved { x, y, width, height, maximized: false })
    }

    #[test]
    fn restores_bounds_that_sit_on_a_connected_screen() {
        assert_eq!(placement_for(at(100.0, 50.0, 1280.0, 800.0), &[MAIN]), at(100.0, 50.0, 1280.0, 800.0));
        assert_eq!(placement_for(at(2000.0, 100.0, 1280.0, 800.0), &[MAIN, SECOND]), at(2000.0, 100.0, 1280.0, 800.0));
    }

    #[test]
    fn drops_bounds_on_a_screen_that_is_no_longer_there_or_nonsense() {
        assert_eq!(placement_for(at(2000.0, 100.0, 1280.0, 800.0), &[MAIN]), None);
        assert_eq!(placement_for(at(-1300.0, 100.0, 1280.0, 800.0), &[MAIN]), None);
        assert_eq!(placement_for(at(100.0, 100.0, 50.0, 800.0), &[MAIN]), None);
        assert_eq!(placement_for(at(f64::NAN, 100.0, 1280.0, 800.0), &[MAIN]), None);
        assert_eq!(placement_for(None, &[MAIN]), None);
        assert_eq!(placement_for(at(1850.0, 1000.0, 1280.0, 800.0), &[MAIN]), None);
        // only a sliver showing
    }

    #[test]
    fn reads_back_what_was_written_and_the_electron_apps_file() {
        let dir = std::env::temp_dir().join(format!("station-window-{}", fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir, FILE), None);
        let saved = Saved { x: 1.0, y: 2.0, width: 800.0, height: 600.0, maximized: true };
        write(&dir, FILE, &saved);
        assert_eq!(read(&dir, FILE), Some(saved));
        // As windowState.js writes it: integers, maximized possibly missing.
        std::fs::write(dir.join(FILE), r#"{ "x": 10, "y": 20, "width": 1280, "height": 800 }"#).unwrap();
        assert_eq!(read(&dir, FILE), at(10.0, 20.0, 1280.0, 800.0));
        assert_eq!(read(&dir, HISTORY_FILE), None); // each window its own file
        let _ = std::fs::remove_dir_all(dir);
    }
}
