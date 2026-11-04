//! Where each window was last: its size and position, and whether it was
//! maximised, kept beside the settings so the app comes back where it was
//! left. The main window's is window-state.json, the Electron app's
//! windowState.js file; the history window has its own.
//!
//! Kept in physical pixels, as Windows places windows. Logical ones went
//! wrong across two screens at different scaling (a laptop at 150% with a
//! monitor at 100%): a window opens on the main screen, so a position saved
//! at the other screen's scale was multiplied by the wrong one, and the
//! window came back far to the side of where it was, or off every screen.
//! A file without `physical` (the Electron app's, or one from before) is in
//! logical pixels at the main screen's scale.
//!
//! A position on no connected screen (a second screen unplugged, a different
//! resolution) is not used: the window keeps its size, made smaller if it no
//! longer fits, and opens in the middle of the main screen.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{PhysicalPosition, PhysicalSize, WebviewWindow, WindowEvent};

/// The main window's; the history window has its own (HISTORY_FILE).
pub const FILE: &str = "window-state.json";
pub const HISTORY_FILE: &str = "history-window-state.json";
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// Px of the window that must sit on some screen.
const MIN_VISIBLE: f64 = 100.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub maximized: bool,
    /// The numbers are physical pixels; without it, logical ones.
    #[serde(default)]
    pub physical: bool,
}

impl Saved {
    /// In physical pixels, a logical file taken at `scale`.
    fn in_physical(self, scale: f64) -> Saved {
        if self.physical {
            return self;
        }
        Saved { x: self.x * scale, y: self.y * scale, width: self.width * scale, height: self.height * scale, maximized: self.maximized, physical: true }
    }
}

/// A screen's work area, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// The bounds to open with: the saved ones while enough of them is on a
/// screen, otherwise the saved size (shrunk to fit) in the middle of the main
/// screen. None when nothing usable was saved.
pub fn placement_for(saved: Option<Saved>, screens: &[Area], main: Option<Area>) -> Option<Saved> {
    let s = saved?;
    if ![s.x, s.y, s.width, s.height].iter().all(|v| v.is_finite()) || s.width < 200.0 || s.height < 150.0 {
        return None;
    }
    let visible = screens.iter().any(|a| {
        let w = (s.x + s.width).min(a.x + a.width) - s.x.max(a.x);
        let h = (s.y + s.height).min(a.y + a.height) - s.y.max(a.y);
        w >= MIN_VISIBLE && h >= MIN_VISIBLE
    });
    if visible {
        return Some(s);
    }
    let a = main.or_else(|| screens.first().copied())?;
    let (width, height) = (s.width.min(a.width), s.height.min(a.height));
    Some(Saved { x: a.x + (a.width - width) / 2.0, y: a.y + (a.height - height) / 2.0, width, height, ..s })
}

fn area(m: &tauri::Monitor) -> Area {
    let area = m.work_area();
    Area { x: f64::from(area.position.x), y: f64::from(area.position.y), width: f64::from(area.size.width), height: f64::from(area.size.height) }
}

/// Puts the window back where it was, then shows it. The window starts
/// hidden so it is never seen jumping there.
pub fn restore(win: &WebviewWindow, dir: &Path, file: &str) {
    let screens: Vec<Area> = win.available_monitors().unwrap_or_default().iter().map(area).collect();
    let main = win.primary_monitor().ok().flatten();
    let scale = main.as_ref().map_or(1.0, |m| m.scale_factor());
    let saved = read(dir, file).map(|s| s.in_physical(scale));
    if let Some(p) = placement_for(saved, &screens, main.as_ref().map(area)) {
        // Moved first: on a screen at another scaling Windows resizes the
        // window as it arrives, and the size set after it is the one kept.
        let _ = win.set_position(PhysicalPosition::new(p.x.round() as i32, p.y.round() as i32));
        let _ = win.set_size(PhysicalSize::new(p.width.round() as u32, p.height.round() as u32));
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
        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
            *state = Saved {
                x: f64::from(pos.x),
                y: f64::from(pos.y),
                width: f64::from(size.width),
                height: f64::from(size.height),
                maximized: state.maximized,
                physical: true,
            };
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
        Some(Saved { x, y, width, height, maximized: false, physical: true })
    }

    #[test]
    fn restores_bounds_that_sit_on_a_connected_screen() {
        assert_eq!(placement_for(at(100.0, 50.0, 1280.0, 800.0), &[MAIN], Some(MAIN)), at(100.0, 50.0, 1280.0, 800.0));
        assert_eq!(placement_for(at(2000.0, 100.0, 1280.0, 800.0), &[MAIN, SECOND], Some(MAIN)), at(2000.0, 100.0, 1280.0, 800.0));
    }

    #[test]
    fn a_window_on_no_screen_keeps_its_size_in_the_middle_of_the_main_one() {
        // The second screen unplugged.
        assert_eq!(placement_for(at(2000.0, 100.0, 1280.0, 800.0), &[MAIN], Some(MAIN)), at(320.0, 120.0, 1280.0, 800.0));
        assert_eq!(placement_for(at(-1300.0, 100.0, 560.0, 820.0), &[MAIN], Some(MAIN)), at(680.0, 110.0, 560.0, 820.0));
        // Only a sliver showing.
        assert_eq!(placement_for(at(1850.0, 1000.0, 1280.0, 800.0), &[MAIN], Some(MAIN)), at(320.0, 120.0, 1280.0, 800.0));
        // Bigger than the main screen: made to fit it.
        assert_eq!(placement_for(at(4000.0, 0.0, 2560.0, 1400.0), &[MAIN], Some(MAIN)), at(0.0, 0.0, 1920.0, 1040.0));
        // Maximised stays maximised, on the main screen.
        let max = Saved { maximized: true, ..at(2000.0, 100.0, 1280.0, 800.0).unwrap() };
        assert!(placement_for(Some(max), &[MAIN], Some(MAIN)).unwrap().maximized);
        // The main screen not said: the first one.
        assert_eq!(placement_for(at(-3000.0, 0.0, 1280.0, 800.0), &[SECOND], None), at(2240.0, 140.0, 1280.0, 800.0));
    }

    #[test]
    fn nonsense_is_not_used() {
        assert_eq!(placement_for(at(100.0, 100.0, 50.0, 800.0), &[MAIN], Some(MAIN)), None);
        assert_eq!(placement_for(at(f64::NAN, 100.0, 1280.0, 800.0), &[MAIN], Some(MAIN)), None);
        assert_eq!(placement_for(None, &[MAIN], Some(MAIN)), None);
        assert_eq!(placement_for(at(5000.0, 0.0, 1280.0, 800.0), &[], None), None);
    }

    #[test]
    fn a_logical_file_is_taken_at_the_main_screens_scale() {
        let logical = Saved { x: 100.0, y: 50.0, width: 1280.0, height: 800.0, maximized: true, physical: false };
        assert_eq!(logical.in_physical(1.5), Saved { x: 150.0, y: 75.0, width: 1920.0, height: 1200.0, maximized: true, physical: true });
        let physical = at(100.0, 50.0, 1280.0, 800.0).unwrap();
        assert_eq!(physical.in_physical(1.5), physical);
    }

    #[test]
    fn reads_back_what_was_written_and_the_electron_apps_file() {
        let dir = std::env::temp_dir().join(format!("station-window-{}", fastrand::u32(..)));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir, FILE), None);
        let saved = Saved { x: 1.0, y: 2.0, width: 800.0, height: 600.0, maximized: true, physical: true };
        write(&dir, FILE, &saved);
        assert_eq!(read(&dir, FILE), Some(saved));
        // As windowState.js writes it: integers, logical, maximized possibly missing.
        std::fs::write(dir.join(FILE), r#"{ "x": 10, "y": 20, "width": 1280, "height": 800 }"#).unwrap();
        assert_eq!(read(&dir, FILE), Some(Saved { x: 10.0, y: 20.0, width: 1280.0, height: 800.0, maximized: false, physical: false }));
        assert_eq!(read(&dir, HISTORY_FILE), None); // each window its own file
        let _ = std::fs::remove_dir_all(dir);
    }
}
