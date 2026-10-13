//! What happens when the scale says a truck has settled: photograph it, read
//! its plate off the full-resolution stream, stamp the weight and plate on
//! the picture, write it all to the history, and keep trying for a better
//! plate while the truck is still on the deck. If the scale then settles
//! heavier (the rest of the truck, or its trailer, rolled on after the first
//! settle), the same entry is corrected: new weight, new picture, the plate
//! kept.
//!
//! Unlike the Electron app there is no vehicle detector: the scale is what
//! says a truck is there, the reader finds every plate in the whole frame,
//! and plate_choice picks the truck's (the biggest, toward the side the
//! settings favour) from among them.
//!
//! The scale's events arrive on a channel. Each weighing then runs as a task
//! of its own, so a reweigh or a cleared deck is heard while the photo is
//! still being taken or the plate retries are going.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use crate::camera::{self, CameraSettings};
use crate::history::{weight_value, Entry, NewEvent};
use crate::plate_choice::{self, Rules};
use crate::scale::frames::format_weight;
use crate::scale::reader::Weighing;
use crate::stamp::{self, Corner, Stamp};
use crate::{log, AppState};

/// A plate this good is not worth retrying for.
const PLATE_CONFIDENT: f64 = 0.9;
const PHOTO_TIMEOUT: Duration = Duration::from_secs(10);
const HD_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug)]
pub enum Event {
    Weighed(Weighing),
    Reweighed(Weighing),
    Cleared,
}

/// The truck on the deck now, and what has been made of it so far.
#[derive(Default)]
struct Current {
    entry_id: Option<String>,
    /// The photo (unstamped) and when it was taken, to re-stamp on a reweigh.
    frame: Option<(Vec<u8>, DateTime<Local>)>,
    plate: Option<Value>,
    /// A heavier settle that arrived before the first one was written.
    pending_reweigh: Option<Weighing>,
}

type Shared = Arc<Mutex<Current>>;

#[derive(Clone)]
struct Pipeline {
    app: AppHandle,
    current: Arc<Mutex<Option<Shared>>>,
    /// One photo at a time; a second truck meanwhile is logged without one.
    busy: Arc<Mutex<bool>>,
}

/// Starts the task; send the scale's events to what comes back.
pub fn start(app: AppHandle) -> mpsc::UnboundedSender<Event> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline { app, current: Arc::default(), busy: Arc::default() };
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let p = pipeline.clone();
            match event {
                Event::Weighed(w) => {
                    // Registered before anything is awaited, so a heavier settle
                    // that lands while the photo is taken is held against it.
                    let current = Shared::default();
                    *p.current.lock().unwrap() = Some(current.clone());
                    tauri::async_runtime::spawn(async move { p.on_weighed(w, current).await });
                }
                Event::Reweighed(w) => {
                    tauri::async_runtime::spawn(async move { p.on_reweighed(w).await });
                }
                Event::Cleared => *p.current.lock().unwrap() = None,
            }
        }
    });
    tx
}

fn object(value: Value) -> Entry {
    match value {
        Value::Object(map) => map,
        _ => Entry::new(),
    }
}

impl Pipeline {
    fn settings(&self) -> Value {
        self.app.state::<AppState>().settings.lock().unwrap().get().clone()
    }

    fn log(&self, message: impl AsRef<str>) {
        log(&self.app, message);
    }

    fn is_current(&self, current: &Shared) -> bool {
        self.current.lock().unwrap().as_ref().is_some_and(|c| Arc::ptr_eq(c, current))
    }

    fn truck_on_deck(&self) -> bool {
        self.app.state::<AppState>().scale.lock().unwrap().as_ref().is_some_and(|s| s.loaded())
    }

    fn record(&self, event: NewEvent, photo: Option<&[u8]>) -> Option<Entry> {
        let state = self.app.state::<AppState>();
        let result = state.history.lock().unwrap().as_ref().map(|h| h.record(event, photo));
        match result {
            Some(Ok(entry)) => Some(entry),
            Some(Err(err)) => {
                self.log(format!("could not write the history: {err}"));
                None
            }
            None => None,
        }
    }

    fn update(&self, id: &str, patch: Entry, photo: Option<&[u8]>) {
        let state = self.app.state::<AppState>();
        let result = state.history.lock().unwrap().as_ref().map(|h| h.update(id, &patch, photo));
        if let Some(Err(err)) = result {
            self.log(format!("could not update the history: {err}"));
        }
    }

    /// The weighing is in the history; a reweigh held meanwhile is applied.
    async fn written(&self, current: &Shared, entry: Option<Entry>) {
        let held = {
            let mut c = current.lock().unwrap();
            c.entry_id = entry.as_ref().and_then(|e| e.get("id")?.as_str().map(String::from));
            c.pending_reweigh.take()
        };
        if let Some(held) = held {
            Box::pin(self.on_reweighed(held)).await;
        }
    }

    fn stamp(&self, frame: &[u8], taken_at: DateTime<Local>, weight_text: &str, plate: Option<&Value>) -> Vec<u8> {
        let s = self.settings();
        let plate_corner = Corner::named(s["plates"]["overlayCorner"].as_str().unwrap_or("top-left"));
        let weight_corner = s["scale"]["overlayCorner"].as_str().map_or(plate_corner.opposite(), Corner::named);
        let plate_text = plate.and_then(|p| p["text"].as_str()).filter(|_| s["plates"]["stampPhoto"] != false);
        let stamp = Stamp {
            taken_at,
            plate: plate_text.map(|t| (t.to_string(), plate_corner)),
            weight: (s["scale"]["stampPhoto"] != false).then(|| (weight_text.to_string(), weight_corner)),
        };
        stamp::stamp_jpeg(frame, &stamp).unwrap_or_else(|err| {
            self.log(format!("could not stamp the photo: {err}"));
            frame.to_vec()
        })
    }

    /// One read of the plate off the HD stream. True when it found a better
    /// one than `current` had.
    async fn try_plate(&self, current: &Shared, camera: &CameraSettings, rules: &Rules, attempt: u32) -> bool {
        let state = self.app.state::<AppState>();
        let Some(hd_url) = camera.url(true) else { return false };
        let hd = match camera::snapshot::capture(&hd_url, "", "", HD_TIMEOUT).await {
            Ok(hd) => hd,
            Err(err) => {
                self.log(format!("plate read failed: {err}"));
                return false;
            }
        };
        let width = camera::snapshot::jpeg_size(&hd).map_or(0.0, |(w, _)| f64::from(w));
        let choice = match state.plates.read(&hd).await {
            Ok(plates) => plate_choice::choose(&plates, width, rules),
            Err(err) => {
                self.log(format!("plate read failed: {err}"));
                return false;
            }
        };
        let Some(read) = choice.plate else {
            match choice.under_floor {
                Some(weak) => self.log(format!("plate \"{}\" at {}%, under the floor (attempt {attempt})", weak.text, (weak.confidence * 100.0).round())),
                None => self.log(format!("no plate found (attempt {attempt})")),
            }
            return false;
        };
        let percent = (read.confidence * 100.0).round();
        let better = current.lock().unwrap().plate.as_ref().is_none_or(|p| read.confidence > p["confidence"].as_f64().unwrap_or(0.0));
        if !better {
            return false;
        }
        let plate = json!({
            "text": read.text,
            "confidence": (read.confidence * 100.0).round() / 100.0,
            "readAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        });
        current.lock().unwrap().plate = Some(plate.clone());
        self.log(format!("plate {} ({percent}%, attempt {attempt})", read.text));
        if let Some(scale) = state.scale.lock().unwrap().as_ref() {
            scale.record_plate(plate.clone());
        }
        let _ = self.app.emit("scale:plate", &plate);
        true
    }

    async fn on_weighed(&self, weighing: Weighing, current: Shared) {
        let s = self.settings();
        if s["scale"]["capturePhoto"] == false {
            return;
        }
        let camera: CameraSettings = serde_json::from_value(s["camera"].clone()).unwrap_or_default();
        let plain = |note: &str| NewEvent {
            weight: Some(weighing.weight),
            unit: Some(weighing.unit.to_string()),
            at: Some(weighing.at),
            note: note.to_string(),
            ..Default::default()
        };
        let Some(photo_url) = camera.url(false) else {
            self.log("weighing with no camera set up - logged without a photo");
            let entry = self.record(plain(""), None);
            return self.written(&current, entry).await;
        };
        // Taken and released in one statement: a std Mutex guard must not be
        // held across an .await.
        let already_busy = std::mem::replace(&mut *self.busy.lock().unwrap(), true);
        if already_busy {
            self.log("a weighing is still being photographed; this one is logged without a picture");
            let entry = self.record(plain("camera busy"), None);
            return self.written(&current, entry).await;
        }

        let weight_text = format_weight(weighing.weight, weighing.unit);
        self.log(format!("{weight_text} on the scale - photographing"));
        match camera::snapshot::capture(&photo_url, "", "", PHOTO_TIMEOUT).await {
            Err(err) => {
                self.log(format!("weighing photo failed: {err}"));
                let entry = self.record(plain(&format!("photo failed: {err}")), None);
                self.written(&current, entry).await;
            }
            Ok(frame) => {
                let taken_at = Local::now();
                current.lock().unwrap().frame = Some((frame.clone(), taken_at));
                let want_plate = s["plates"]["enabled"] != false && s["scale"]["readPlate"] != false;
                let rules: Rules = serde_json::from_value(s["plates"].clone()).unwrap_or_default();
                if want_plate {
                    self.try_plate(&current, &camera, &rules, 1).await;
                }
                let plate = current.lock().unwrap().plate.clone();
                let photo = self.stamp(&frame, taken_at, &weight_text, plate.as_ref());
                let event = NewEvent { plate: plate.clone(), ..plain("") };
                let entry = self.record(event, Some(&photo));
                let entry_id = entry.as_ref().and_then(|e| e.get("id")?.as_str().map(String::from));
                self.written(&current, entry).await;

                // Keep trying for a better plate while the truck is still there.
                if let (true, Some(entry_id)) = (want_plate, entry_id) {
                    let attempts = s["plates"]["retryAttempts"].as_u64().filter(|&n| n > 0).unwrap_or(10) as u32;
                    let wait = Duration::from_secs(s["plates"]["retrySeconds"].as_u64().filter(|&n| n > 0).unwrap_or(3));
                    for attempt in 2..=attempts {
                        let good_enough = current.lock().unwrap().plate.as_ref().is_some_and(|p| p["confidence"].as_f64().unwrap_or(0.0) >= PLATE_CONFIDENT);
                        if good_enough {
                            break;
                        }
                        tokio::time::sleep(wait).await;
                        if !self.truck_on_deck() || !self.is_current(&current) {
                            break; // the deck cleared, or another truck is being weighed
                        }
                        if self.try_plate(&current, &camera, &rules, attempt).await {
                            let plate = current.lock().unwrap().plate.clone();
                            self.update(&entry_id, object(json!({ "plate": plate })), None);
                        }
                    }
                }
            }
        }
        *self.busy.lock().unwrap() = false;
    }

    /// More of the same truck on the deck: the entry keeps its id, time and
    /// plate and takes the new weight, with a fresh picture of the whole
    /// truck if the camera gives one, or the first picture re-stamped if not.
    async fn on_reweighed(&self, weighing: Weighing) {
        let Some(current) = self.current.lock().unwrap().clone() else { return };
        let (entry_id, frame, plate) = {
            let mut c = current.lock().unwrap();
            let Some(entry_id) = c.entry_id.clone() else {
                // Still photographing the first settle; applied once written.
                c.pending_reweigh = Some(weighing);
                return;
            };
            (entry_id, c.frame.clone(), c.plate.clone())
        };
        let weight_text = format_weight(weighing.weight, weighing.unit);
        self.log(format!("{weight_text} on the scale - more of the truck; correcting {}", format_weight(weighing.previous.unwrap_or_default(), weighing.unit)));
        let at = DateTime::from_timestamp_millis(weighing.at).map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true));
        let patch = object(json!({
            "weight": weight_value(weighing.weight),
            "unit": weighing.unit.to_string(),
            "corrected": { "from": weighing.previous.map(weight_value), "at": at },
        }));
        let camera: CameraSettings = serde_json::from_value(self.settings()["camera"].clone()).unwrap_or_default();
        let (Some((first, first_at)), Some(url)) = (frame, camera.url(false)) else {
            return self.update(&entry_id, patch, None);
        };
        let (frame, taken_at) = match camera::snapshot::capture(&url, "", "", PHOTO_TIMEOUT).await {
            Ok(frame) => (frame, Local::now()),
            Err(err) => {
                self.log(format!("could not re-photograph the truck ({err}); re-stamping the first picture"));
                (first, first_at)
            }
        };
        current.lock().unwrap().frame = Some((frame.clone(), taken_at));
        let photo = self.stamp(&frame, taken_at, &weight_text, plate.as_ref());
        self.update(&entry_id, patch, Some(&photo));
    }
}

/// The Plates tab's "Read now": whatever is in front of the camera.
pub async fn read_plate_now(app: &AppHandle) -> Result<Value, String> {
    use base64::Engine;
    let state = app.state::<AppState>();
    let camera: CameraSettings = state.settings.lock().unwrap().section("camera");
    let (Some(url), Some(hd_url)) = (camera.url(false), camera.url(true)) else { return Err("No camera stream is set up yet".into()) };
    let started = std::time::Instant::now();
    let frame = camera::snapshot::capture(&url, "", "", PHOTO_TIMEOUT).await?;
    let hd = if hd_url == url { frame.clone() } else { camera::snapshot::capture(&hd_url, "", "", HD_TIMEOUT).await? };
    let (frame_width, frame_height) = camera::snapshot::jpeg_size(&hd).unwrap_or((0, 0));
    let rules: Rules = state.settings.lock().unwrap().section("plates");
    let choice = plate_choice::choose(&state.plates.read(&hd).await?, f64::from(frame_width), &rules);
    let plate = choice
        .plate
        .map(|p| json!({ "text": p.text, "confidence": (p.confidence * 100.0).round() / 100.0, "frameWidth": frame_width, "frameHeight": frame_height }));
    let error = match (&plate, &choice.under_floor) {
        (Some(_), _) => String::new(),
        (None, Some(weak)) => format!("Only \"{}\" at {}%, under the minimum confidence.", weak.text, (weak.confidence * 100.0).round()),
        (None, None) => "No plate found in this frame.".into(),
    };
    Ok(json!({
        "error": error,
        "plate": plate,
        "image": format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&frame)),
        "ms": started.elapsed().as_millis() as u64,
        "reader": state.plates.status(),
    }))
}
