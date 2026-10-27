//! The station's settings: one JSON file under the app's data folder, read on
//! start, written on every save. Defaults mirror the scale camera card on the
//! Falcon site so a fresh install behaves like the yard's.
//!
//! Kept as loose JSON rather than one big struct, as the Electron side did:
//! the window saves whatever shape it likes, a field this side does not know
//! yet survives a round trip, and each part of the app reads its own section
//! into a typed struct (`ScaleSettings` and so on) when it needs it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

pub fn defaults() -> Value {
    let mut d = base_defaults();
    d["alerts"] = crate::alerts::defaults_json();
    d
}

fn base_defaults() -> Value {
    json!({
        "camera": {
            "name": "Scale",
            "host": "",
            "username": "admin",
            "password": "",
            "streamUrl": "",
            "hdStreamUrl": "",
            "snapshotUrl": "",
            "onvifPort": 80,
            "showClock": true
        },
        "plates": {
            "enabled": true,
            "minConfidence": 0.6,
            "overlayCorner": "top-left",
            "retryAttempts": 90,
            "retrySeconds": 1,
            "stampPhoto": true,
            "requireDigit": true,
            "ignoreWords": "",
            "pick": "largest",
            "favorSide": "right",
            "favorPercent": 50
        },
        "scale": {
            "enabled": false,
            "host": "",
            "port": 950,
            "unit": "lb",
            "share": true,
            "minWeight": 4000,
            "clearWeight": 1000,
            "stableSeconds": 2,
            "stableTolerance": 40,
            "unsettledSeconds": 20,
            "unsettledTolerance": 200,
            "display": "picture",
            "overlayCorner": "bottom-left",
            "stampPhoto": true,
            "capturePhoto": true,
            "readPlate": true,
            "soundOnWeighing": true
        },
        "history": {
            "retentionDays": 7,
            "folder": "",
            "savePhotos": true,
            "saveLog": true,
            "photoQuality": 85
        },
        "window": {
            "fullscreen": false,
            "alwaysOnTop": false,
            "startWithWindows": false
        },
        "updates": {
            "channel": "stable"
        },
        "support": {
            "relayUrl": "https://scale-station-reports.joepowles.workers.dev",
            "repo": "Joepowles/fgr-scale-station",
            "githubToken": "",
            "autoReport": true
        },
        "meta": { "version": 4 }
    })
}

/// `over` laid on top of `base`, objects merged key by key, anything else
/// replaced outright.
pub fn merge(base: &Value, over: &Value) -> Value {
    let (Value::Object(base_map), Value::Object(over_map)) = (base, over) else {
        return base.clone();
    };
    let mut out: Map<String, Value> = base_map.clone();
    for (key, value) in over_map {
        let merged = match (base_map.get(key), value) {
            (Some(b @ Value::Object(_)), Value::Object(_)) => merge(b, value),
            _ => value.clone(),
        };
        out.insert(key.clone(), merged);
    }
    Value::Object(out)
}

fn version_of(settings: &Value) -> u64 {
    settings.pointer("/meta/version").and_then(Value::as_u64).filter(|&v| v > 0).unwrap_or(1)
}

/// Settings saved by an older version carry the old defaults as if chosen.
/// Where they still match those, they move to the new ones; a corner the user
/// picked on purpose is left alone.
fn migrate(mut settings: Value, saved_version: u64) -> Value {
    if saved_version < 2 {
        let mut move_corner = |pointer: &str, from: &str, to: &str| {
            if let Some(v) = settings.pointer_mut(pointer) {
                if v == from {
                    *v = to.into();
                }
            }
        };
        move_corner("/plates/overlayCorner", "bottom-left", "top-left");
        move_corner("/scale/overlayCorner", "bottom-right", "bottom-left");
    }
    if saved_version < 3 {
        // Tries after the settle became pictures from the moment the truck
        // rolls on: ten, three seconds apart, missed a truck pulling up.
        let mut move_default = |pointer: &str, from: i64, to: i64| {
            if let Some(v) = settings.pointer_mut(pointer) {
                if v.as_f64() == Some(from as f64) {
                    *v = to.into();
                }
            }
        };
        move_default("/plates/retryAttempts", 10, 90);
        move_default("/plates/retrySeconds", 3, 1);
    }
    if saved_version == 3 {
        // 30 pictures stopped the first yard truck just as its plate came
        // into reach, 28 s after it rolled on.
        if let Some(v) = settings.pointer_mut("/plates/retryAttempts") {
            if v.as_f64() == Some(30.0) {
                *v = 90.into();
            }
        }
    }
    settings["meta"]["version"] = version_of(&defaults()).into();
    settings
}

pub const SETTINGS_FILE: &str = "settings.json";

pub struct SettingsStore {
    file: PathBuf,
    current: Value,
}

impl SettingsStore {
    pub fn load(folder: &Path) -> Self {
        let _ = fs::create_dir_all(folder);
        let file = folder.join(SETTINGS_FILE);
        let saved = fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok());
        let current = match saved {
            Some(raw) => {
                // A file from before meta existed is version 1, whatever
                // merge() would fill in; migrate sees the real saved values.
                let saved_version = version_of(&raw);
                let current = migrate(merge(&defaults(), &raw), saved_version);
                if saved_version < version_of(&defaults()) {
                    let _ = write_atomic(&file, &current);
                }
                current
            }
            None => defaults(),
        };
        Self { file, current }
    }

    pub fn get(&self) -> &Value {
        &self.current
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn save(&mut self, patch: &Value) -> std::io::Result<&Value> {
        let next = merge(&self.current, patch);
        write_atomic(&self.file, &next)?;
        self.current = next;
        Ok(&self.current)
    }

    /// One section read into the struct that part of the app wants, with the
    /// struct's defaults for anything missing or unreadable.
    pub fn section<T: serde::de::DeserializeOwned + Default>(&self, name: &str) -> T {
        self.current.get(name).cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
    }
}

/// Written beside and renamed over, so a crash mid-write leaves the old file.
fn write_atomic(file: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = file.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
    fs::rename(tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("station-settings-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_fresh_install_gets_the_defaults() {
        let store = SettingsStore::load(&scratch("fresh"));
        assert_eq!(store.get(), &defaults());
    }

    #[test]
    fn a_save_merges_into_its_section_and_survives_a_reload() {
        let dir = scratch("save");
        let mut store = SettingsStore::load(&dir);
        store.save(&json!({ "scale": { "host": "192.168.1.9", "enabled": true } })).unwrap();
        let again = SettingsStore::load(&dir);
        assert_eq!(again.get()["scale"]["host"], "192.168.1.9");
        assert_eq!(again.get()["scale"]["port"], 950);
        assert_eq!(again.get()["camera"]["username"], "admin");
    }

    #[test]
    fn old_picture_defaults_move_but_chosen_ones_stay() {
        let migrated = migrate(merge(&defaults(), &json!({ "plates": { "retryAttempts": 10, "retrySeconds": 3 }, "meta": { "version": 2 } })), 2);
        assert_eq!(migrated["plates"]["retryAttempts"], 90);
        assert_eq!(migrated["plates"]["retrySeconds"], 1);
        let chosen = migrate(merge(&defaults(), &json!({ "plates": { "retryAttempts": 12, "retrySeconds": 2 }, "meta": { "version": 2 } })), 2);
        assert_eq!(chosen["plates"]["retryAttempts"], 12);
        assert_eq!(chosen["plates"]["retrySeconds"], 2);
        assert_eq!(chosen["meta"]["version"], 4);
        let beta = migrate(merge(&defaults(), &json!({ "plates": { "retryAttempts": 30 }, "meta": { "version": 3 } })), 3);
        assert_eq!(beta["plates"]["retryAttempts"], 90);
    }

    #[test]
    fn old_default_corners_move_but_chosen_ones_stay() {
        let dir = scratch("migrate");
        fs::create_dir_all(&dir).unwrap();
        let v1 = json!({ "plates": { "overlayCorner": "bottom-left" }, "scale": { "overlayCorner": "top-right" } });
        fs::write(dir.join("settings.json"), v1.to_string()).unwrap();
        let store = SettingsStore::load(&dir);
        assert_eq!(store.get()["plates"]["overlayCorner"], "top-left");
        assert_eq!(store.get()["scale"]["overlayCorner"], "top-right");
        assert_eq!(store.get()["meta"]["version"], 4);
        // and the migrated file was written back
        let on_disk: Value = serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(on_disk["meta"]["version"], 4);
    }

    #[test]
    fn the_scale_section_reads_into_its_struct() {
        let mut store = SettingsStore::load(&scratch("section"));
        store.save(&json!({ "scale": { "host": "10.0.0.5", "minWeight": "" } })).unwrap();
        let scale: crate::scale::reader::ScaleSettings = store.section("scale");
        assert_eq!(scale.host, "10.0.0.5");
        assert_eq!(scale.port, 950.0);
        assert_eq!(scale.trigger.min_weight, 0.0);
    }
}
