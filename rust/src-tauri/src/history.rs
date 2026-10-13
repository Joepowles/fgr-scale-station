//! The local history: one JSON line per event in history.jsonl, photos beside
//! it in photos/, both under the folder the History tab points at. Entries
//! older than the retention period are dropped once an hour, photos and all.
//!
//! Pure file handling, no database, so the folder can be opened in Explorer,
//! copied to a USB stick, or pointed at a network share.
//!
//! Entries are kept as loose JSON objects rather than a struct: an update
//! merges in whatever fields the caller has (a better plate, a `corrected`
//! weight), and a line written by the Electron app reads back unchanged.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const LOG_FILE: &str = "history.jsonl";
pub const PHOTO_DIR: &str = "photos";
pub const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

pub type Entry = Map<String, Value>;

/// Called with every entry written or changed, so the window can show it.
pub type OnEntry = Box<dyn Fn(&Entry) + Send + Sync>;

/// The history section of the settings, as far as this file cares.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistorySettings {
    #[serde(deserialize_with = "crate::scale::frames::lenient_number")]
    pub retention_days: f64,
    pub save_photos: bool,
    pub save_log: bool,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self { retention_days: 7.0, save_photos: true, save_log: true }
    }
}

/// What a caller has to say about a new event.
#[derive(Debug, Default, Clone)]
pub struct NewEvent {
    pub kind: Option<String>,
    pub weight: Option<f64>,
    pub unit: Option<String>,
    /// Milliseconds since the epoch; now when None.
    pub at: Option<i64>,
    pub plate: Option<Value>,
    pub vehicle: Option<Value>,
    pub note: String,
}

#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pruned {
    pub dropped: usize,
    pub photos_removed: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub entries: usize,
    pub photo_bytes: u64,
    pub oldest: Option<Value>,
    pub newest: Option<Value>,
    pub folder: PathBuf,
}

pub struct History {
    folder: PathBuf,
    retention_days: f64,
    save_photos: bool,
    save_log: bool,
    on_entry: OnEntry,
}

/// "20261006-142010": the local time an entry's id and photo name start with.
fn stamp(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%Y%m%d-%H%M%S").to_string()
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A weight as JSON the way the Electron app wrote it: 48320, not 48320.0.
pub fn weight_value(weight: f64) -> Value {
    if weight.fract() == 0.0 && weight.abs() < 1e15 {
        json!(weight as i64)
    } else {
        json!(weight)
    }
}

fn time_of(entry: &Entry) -> Option<i64> {
    let at = entry.get("at")?.as_str()?;
    Some(DateTime::parse_from_rfc3339(at).ok()?.timestamp_millis())
}

impl History {
    pub fn new(folder: PathBuf, settings: &HistorySettings, on_entry: OnEntry) -> std::io::Result<Self> {
        let mut history = Self { folder: PathBuf::new(), retention_days: 7.0, save_photos: true, save_log: true, on_entry };
        history.configure(folder, settings)?;
        Ok(history)
    }

    pub fn configure(&mut self, folder: PathBuf, settings: &HistorySettings) -> std::io::Result<()> {
        fs::create_dir_all(folder.join(PHOTO_DIR))?;
        self.folder = folder;
        self.retention_days = if settings.retention_days >= 1.0 { settings.retention_days } else { 7.0 };
        self.save_photos = settings.save_photos;
        self.save_log = settings.save_log;
        Ok(())
    }

    #[cfg(test)]
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    fn log_path(&self) -> PathBuf {
        self.folder.join(LOG_FILE)
    }

    /// Where a photo of that name lives, or None for a name that would reach
    /// outside the photos folder ("../settings.json" and the like).
    pub fn photo_path(&self, name: &str) -> Option<PathBuf> {
        let plain = Path::new(name).file_name().is_some_and(|n| n == name);
        plain.then(|| self.folder.join(PHOTO_DIR).join(name))
    }

    /// Writes an event. `photo` is a JPEG; it is saved beside the log and the
    /// entry carries its file name. Returns the entry as written.
    pub fn record(&self, event: NewEvent, photo: Option<&[u8]>) -> std::io::Result<Entry> {
        let at = event.at.and_then(DateTime::from_timestamp_millis).unwrap_or_else(Utc::now);
        let id = format!("{}-{}", stamp(at), (0..5).map(|_| fastrand::alphanumeric().to_ascii_lowercase()).collect::<String>());
        let mut entry = json!({
            "id": id,
            "at": iso(at),
            "type": event.kind.unwrap_or_else(|| "weighing".into()),
            "weight": event.weight.map(weight_value),
            "unit": event.unit.unwrap_or_else(|| "lb".into()),
            "plate": event.plate,
            "vehicle": event.vehicle,
            "photo": null,
            "note": event.note,
        });
        if let (Some(photo), true) = (photo, self.save_photos) {
            let name = format!("{id}.jpg");
            fs::write(self.folder.join(PHOTO_DIR).join(&name), photo)?;
            entry["photo"] = name.into();
        }
        let Value::Object(entry) = entry else { unreachable!("json! of an object") };
        if self.save_log {
            let mut log = OpenOptions::new().create(true).append(true).open(self.log_path())?;
            writeln!(log, "{}", Value::Object(entry.clone()))?;
        }
        (self.on_entry)(&entry);
        Ok(entry)
    }

    /// Changes fields on an entry already written (a better plate read, a
    /// corrected weight). A photo replaces the entry's picture under the same
    /// name. The log is rewritten; it is small, one line per truck.
    pub fn update(&self, id: &str, patch: &Entry, photo: Option<&[u8]>) -> std::io::Result<Option<Entry>> {
        let mut entries = self.read();
        let Some(entry) = entries.iter_mut().find(|e| e.get("id").and_then(Value::as_str) == Some(id)) else {
            return Ok(None);
        };
        entry.extend(patch.clone());
        if let (Some(photo), true) = (photo, self.save_photos) {
            let name = entry.get("photo").and_then(Value::as_str).map_or_else(|| format!("{id}.jpg"), str::to_string);
            if let Some(path) = self.photo_path(&name) {
                fs::write(path, photo)?;
                entry.insert("photo".into(), name.into());
            }
        }
        let updated = entry.clone();
        self.write_all(&entries)?;
        (self.on_entry)(&updated);
        Ok(Some(updated))
    }

    /// Every entry, oldest first. A line that does not parse is skipped.
    pub fn read(&self) -> Vec<Entry> {
        let Ok(text) = fs::read_to_string(self.log_path()) else { return Vec::new() };
        text.lines().filter_map(|line| serde_json::from_str::<Entry>(line).ok()).collect()
    }

    /// Newest first, limited.
    pub fn list(&self, limit: usize) -> Vec<Entry> {
        let mut entries = self.read();
        entries.reverse();
        entries.truncate(limit);
        entries
    }

    /// Written beside and renamed over, so a crash mid-write leaves the old log.
    fn write_all(&self, entries: &[Entry]) -> std::io::Result<()> {
        let tmp = self.folder.join(format!("{LOG_FILE}.tmp"));
        let mut text = String::new();
        for entry in entries {
            text.push_str(&serde_json::to_string(entry)?);
            text.push('\n');
        }
        fs::write(&tmp, text)?;
        fs::rename(tmp, self.log_path())
    }

    /// Drops what is older than the retention period: entries from the log
    /// and the photos they named.
    ///
    /// While the log is kept, a photo no entry names is an orphan and goes
    /// too. Without the log, the photos are the only record and nothing names
    /// them, so they go by age instead. (The Electron version removed every
    /// unnamed photo, which with the log turned off was every photo, within
    /// the hour.)
    pub fn prune(&self, now_ms: i64) -> Pruned {
        let cutoff = now_ms - (self.retention_days * DAY_MS as f64) as i64;
        let entries = self.read();
        let (keep, gone): (Vec<Entry>, Vec<Entry>) = entries.into_iter().partition(|e| time_of(e).is_none_or(|at| at >= cutoff));
        if !gone.is_empty() {
            let _ = self.write_all(&keep);
        }
        let named: std::collections::HashSet<&str> = keep.iter().filter_map(|e| e.get("photo")?.as_str()).collect();
        let cutoff_time = SystemTime::UNIX_EPOCH + Duration::from_millis(cutoff.max(0) as u64);
        let mut photos_removed = 0;
        for file in fs::read_dir(self.folder.join(PHOTO_DIR)).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().ends_with(".jpg") || named.contains(name.as_str()) {
                continue;
            }
            let old = file.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff_time);
            if (self.save_log || old) && fs::remove_file(file.path()).is_ok() {
                photos_removed += 1;
            }
        }
        Pruned { dropped: gone.len(), photos_removed }
    }

    /// Sizes for the History tab.
    pub fn stats(&self) -> Stats {
        let entries = self.read();
        let photo_bytes = fs::read_dir(self.folder.join(PHOTO_DIR)).into_iter().flatten().flatten().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum();
        Stats {
            entries: entries.len(),
            photo_bytes,
            oldest: entries.first().and_then(|e| e.get("at").cloned()),
            newest: entries.last().and_then(|e| e.get("at").cloned()),
            folder: self.folder.clone(),
        }
    }
}

// Ported from tests/history.test.js, plus the photos-without-a-log case.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("station-history-{}-{}", std::process::id(), fastrand::u64(..)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn history_with(settings: HistorySettings) -> History {
        History::new(scratch(), &settings, Box::new(|_| {})).unwrap()
    }

    fn history() -> History {
        history_with(HistorySettings::default())
    }

    fn at(iso: &str) -> Option<i64> {
        Some(DateTime::parse_from_rfc3339(iso).unwrap().timestamp_millis())
    }

    fn weighing(weight: f64) -> NewEvent {
        NewEvent { weight: Some(weight), unit: Some("lb".into()), ..Default::default() }
    }

    fn patch(value: Value) -> Entry {
        value.as_object().unwrap().clone()
    }

    fn photo_of(e: &Entry) -> String {
        e["photo"].as_str().unwrap().to_string()
    }

    #[test]
    fn records_entries_with_photos_and_lists_newest_first() {
        let h = history();
        let a = h.record(NewEvent { at: at("2026-10-06T14:20:10Z"), ..weighing(12200.0) }, Some(b"jpeg-a")).unwrap();
        let plate = json!({ "text": "ABC123", "confidence": 0.93 });
        let b = h.record(NewEvent { at: at("2026-10-06T15:00:00Z"), plate: Some(plate), ..weighing(48320.0) }, Some(b"jpeg-b")).unwrap();
        assert!(h.photo_path(&photo_of(&a)).unwrap().exists());
        let list = h.list(200);
        assert_eq!(list.iter().map(|e| e["weight"].as_f64().unwrap()).collect::<Vec<_>>(), [48320.0, 12200.0]);
        assert_eq!(list[0]["plate"]["text"], "ABC123");
        assert!(photo_of(&b).ends_with(".jpg"));
        assert_eq!(a["at"], "2026-10-06T14:20:10.000Z");
        assert_eq!(a["type"], "weighing");
    }

    #[test]
    fn updates_an_entry_in_place() {
        let h = history();
        let e = h.record(weighing(1000.0), None).unwrap();
        h.update(e["id"].as_str().unwrap(), &patch(json!({ "plate": { "text": "NEW", "confidence": 0.95 } })), None).unwrap();
        assert_eq!(h.list(200)[0]["plate"]["text"], "NEW");
        assert_eq!(h.read().len(), 1);
    }

    #[test]
    fn prunes_entries_and_photos_past_the_retention_period() {
        let h = history();
        let now = Utc::now().timestamp_millis();
        let old = h.record(NewEvent { at: Some(now - 10 * DAY_MS), ..weighing(1.0) }, Some(b"old")).unwrap();
        let fresh = h.record(weighing(2.0), Some(b"new")).unwrap();
        fs::write(h.photo_path("orphan.jpg").unwrap(), "x").unwrap();
        let result = h.prune(now);
        assert_eq!(result.dropped, 1);
        assert!(!h.photo_path(&photo_of(&old)).unwrap().exists());
        assert!(h.photo_path(&photo_of(&fresh)).unwrap().exists());
        assert!(!h.photo_path("orphan.jpg").unwrap().exists());
        assert_eq!(h.list(200).iter().map(|e| e["weight"].as_f64().unwrap()).collect::<Vec<_>>(), [2.0]);
    }

    #[test]
    fn can_keep_the_log_without_photos() {
        let h = history_with(HistorySettings { save_photos: false, ..Default::default() });
        let e = h.record(weighing(5.0), Some(b"x")).unwrap();
        assert_eq!(e["photo"], Value::Null);
        assert_eq!(fs::read_dir(h.folder().join(PHOTO_DIR)).unwrap().count(), 0);
    }

    #[test]
    fn can_keep_photos_without_the_log_and_they_survive_a_prune() {
        let h = history_with(HistorySettings { save_log: false, ..Default::default() });
        let e = h.record(weighing(5.0), Some(b"truck")).unwrap();
        assert!(h.read().is_empty());
        assert_eq!(h.prune(Utc::now().timestamp_millis()), Pruned::default());
        assert!(h.photo_path(&photo_of(&e)).unwrap().exists());
        // ...until they are past the retention period themselves.
        let later = Utc::now().timestamp_millis() + 8 * DAY_MS;
        assert_eq!(h.prune(later).photos_removed, 1);
    }

    #[test]
    fn an_update_can_replace_the_photo_under_the_same_name() {
        let h = history();
        let e = h.record(weighing(4740.0), Some(b"front-axle")).unwrap();
        let updated =
            h.update(e["id"].as_str().unwrap(), &patch(json!({ "weight": 38200, "corrected": { "from": 4740 } })), Some(b"whole-truck")).unwrap().unwrap();
        assert_eq!(updated["photo"], e["photo"]);
        assert_eq!(fs::read_to_string(h.photo_path(&photo_of(&e)).unwrap()).unwrap(), "whole-truck");
        assert_eq!(h.list(200)[0]["weight"], 38200);
        assert_eq!(h.read().len(), 1);
    }

    #[test]
    fn photo_names_cannot_leave_the_photos_folder() {
        let h = history();
        assert!(h.photo_path("20261006-142010-abcde.jpg").is_some());
        assert!(h.photo_path("../settings.json").is_none());
        assert!(h.photo_path("/etc/passwd").is_none());
        assert!(h.photo_path("").is_none());
    }

    #[test]
    fn reads_a_log_written_by_the_electron_app() {
        let h = history();
        let line = r#"{"id":"20261006-142010-x1y2z","at":"2026-10-06T14:20:10.000Z","type":"weighing","weight":12200,"unit":"lb","plate":null,"vehicle":null,"photo":null,"note":"","corrected":{"from":4740}}"#;
        fs::write(h.folder().join(LOG_FILE), format!("{line}\nnot json\n")).unwrap();
        let entries = h.read();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["corrected"]["from"], 4740);
    }

    #[test]
    fn tells_the_window_about_each_entry() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let h = History::new(scratch(), &HistorySettings::default(), Box::new(move |e| s.lock().unwrap().push(e["weight"].clone()))).unwrap();
        let e = h.record(weighing(100.0), None).unwrap();
        h.update(e["id"].as_str().unwrap(), &patch(json!({ "weight": 200 })), None).unwrap();
        assert_eq!(*seen.lock().unwrap(), [json!(100), json!(200)]);
    }

    #[test]
    fn writes_lines_like_the_electron_app_did() {
        let h = history();
        h.record(NewEvent { at: at("2026-10-06T14:20:10Z"), ..weighing(48320.0) }, None).unwrap();
        let line = fs::read_to_string(h.folder().join(LOG_FILE)).unwrap();
        let expected =
            r#"{"id":"ID","at":"2026-10-06T14:20:10.000Z","type":"weighing","weight":48320,"unit":"lb","plate":null,"vehicle":null,"photo":null,"note":""}"#;
        let id = &line[7..28];
        assert_eq!(line.trim_end(), expected.replace("ID", id));
    }

    #[test]
    fn stats_count_entries_and_photo_bytes() {
        let h = history();
        h.record(NewEvent { at: at("2026-10-06T14:00:00Z"), ..weighing(1.0) }, Some(b"12345")).unwrap();
        h.record(NewEvent { at: at("2026-10-06T15:00:00Z"), ..weighing(2.0) }, Some(b"123")).unwrap();
        let s = h.stats();
        assert_eq!((s.entries, s.photo_bytes), (2, 8));
        assert_eq!(s.oldest, Some(json!("2026-10-06T14:00:00.000Z")));
        assert_eq!(s.newest, Some(json!("2026-10-06T15:00:00.000Z")));
    }
}
