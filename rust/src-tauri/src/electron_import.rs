//! Moving over from the Electron app on the same PC.
//!
//! The installer takes the Electron app off the PC but keeps its data
//! (windows/installer-hooks.nsh). On the Rust app's first start - no
//! settings of its own yet - this reads that data, so the PC carries on as it
//! was: the camera and scale set up, the window where it was, the history
//! and its photos where they were, the log going on from where it stopped.
//!
//! The history is not copied: the Rust settings point at the Electron app's
//! history folder, which has the same layout, so going back to the Electron
//! app by hand finds every weighing made meanwhile.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::settings::SETTINGS_FILE;
use crate::window_state;

/// Where the Electron app kept its data: Electron's userData, named after
/// the product, under the same config folder (%APPDATA% on Windows).
pub fn electron_data_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("Falcon Scale Station")
}

/// What was brought over, for the log. None when there was nothing to do.
#[derive(Debug, PartialEq)]
pub struct Imported {
    pub from: PathBuf,
    pub history_folder: Option<String>,
}

/// Copies the Electron app's settings into `data_dir` when the Rust app has
/// none yet, with the window place and the log. Never overwrites anything.
pub fn import(electron_dir: &Path, data_dir: &Path) -> Result<Option<Imported>, String> {
    let ours = data_dir.join(SETTINGS_FILE);
    let theirs = electron_dir.join(SETTINGS_FILE);
    if ours.exists() || !theirs.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&theirs).map_err(|e| format!("could not read {}: {e}", theirs.display()))?;
    let mut settings: Value = serde_json::from_str(&text).map_err(|e| format!("{} is not readable: {e}", theirs.display()))?;
    if !settings.is_object() {
        return Err(format!("{} holds no settings", theirs.display()));
    }
    // The default history folder is "history" under the app's data folder;
    // that was the Electron app's, so it is named outright.
    let mut history_folder = None;
    let chosen = settings.pointer("/history/folder").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if chosen.is_empty() && electron_dir.join("history").is_dir() {
        let folder = electron_dir.join("history").display().to_string();
        if !settings["history"].is_object() {
            settings["history"] = serde_json::json!({});
        }
        settings["history"]["folder"] = folder.clone().into();
        history_folder = Some(folder);
    }
    fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let pretty = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fs::write(&ours, pretty).map_err(|e| format!("could not write {}: {e}", ours.display()))?;
    for name in [window_state::FILE, "station.log"] {
        let (from, to) = (electron_dir.join(name), data_dir.join(name));
        if from.is_file() && !to.exists() {
            let _ = fs::copy(from, to);
        }
    }
    Ok(Some(Imported { from: electron_dir.to_path_buf(), history_folder }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("station-import-{name}-{}", fastrand::u32(..)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn brings_the_electron_apps_settings_window_and_log_and_points_at_its_history() {
        let (electron, ours) = (temp("electron"), temp("ours"));
        fs::write(electron.join(SETTINGS_FILE), r#"{ "camera": { "host": "10.0.0.5" }, "history": { "retentionDays": 14, "folder": "" } }"#).unwrap();
        fs::write(electron.join(window_state::FILE), r#"{ "x": 10, "y": 20, "width": 1280, "height": 800 }"#).unwrap();
        fs::write(electron.join("station.log"), "2026-10-12T10:00:00.000Z deck clear\n").unwrap();
        fs::create_dir_all(electron.join("history").join("photos")).unwrap();

        let imported = import(&electron, &ours).unwrap().expect("imported");
        let history = electron.join("history").display().to_string();
        assert_eq!(imported.history_folder.as_deref(), Some(history.as_str()));
        let settings: Value = serde_json::from_str(&fs::read_to_string(ours.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(settings["camera"]["host"], "10.0.0.5");
        assert_eq!(settings["history"]["retentionDays"], 14);
        assert_eq!(settings["history"]["folder"], history);
        assert!(ours.join(window_state::FILE).is_file());
        assert!(fs::read_to_string(ours.join("station.log")).unwrap().contains("deck clear"));

        // Once there are settings of its own, never again.
        fs::write(electron.join(SETTINGS_FILE), r#"{ "camera": { "host": "10.9.9.9" } }"#).unwrap();
        assert_eq!(import(&electron, &ours).unwrap(), None);
        let _ = (fs::remove_dir_all(electron), fs::remove_dir_all(ours));
    }

    #[test]
    fn a_history_folder_chosen_on_purpose_is_kept() {
        let (electron, ours) = (temp("electron"), temp("ours"));
        fs::write(electron.join(SETTINGS_FILE), r#"{ "history": { "folder": "D:\\Scale history" } }"#).unwrap();
        fs::create_dir_all(electron.join("history")).unwrap();
        assert_eq!(import(&electron, &ours).unwrap().unwrap().history_folder, None);
        let settings: Value = serde_json::from_str(&fs::read_to_string(ours.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!(settings["history"]["folder"], "D:\\Scale history");
        let _ = (fs::remove_dir_all(electron), fs::remove_dir_all(ours));
    }

    #[test]
    fn nothing_to_import_is_not_an_error_and_a_broken_file_says_so() {
        let (electron, ours) = (temp("electron"), temp("ours"));
        assert_eq!(import(&electron, &ours).unwrap(), None);
        fs::write(electron.join(SETTINGS_FILE), "{ not json").unwrap();
        assert!(import(&electron, &ours).unwrap_err().contains("not readable"));
        assert!(!ours.join(SETTINGS_FILE).exists());
        let _ = (fs::remove_dir_all(electron), fs::remove_dir_all(ours));
    }
}
