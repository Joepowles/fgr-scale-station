//! What happens to a truck on the scale: photograph it, read its plate,
//! stamp the weight and plate on the picture, and write it to the history.
//!
//! It starts when the truck rolls onto the deck (the scale's Arrived), not
//! when it settles: the plate is readable as the truck comes on, and often
//! not by the time it has pulled up. From then until the deck clears, one
//! connection to the full-resolution stream (camera/burst.rs) hands out
//! frames, and each frame's plates are read. truck_photos.rs adds them up:
//! the plate read most surely across the frames, and the frame where that
//! plate was largest - the truck closest while still readable - as the
//! photo, in full resolution. The history entry is written when the truck
//! settles, and rewritten, stamp and all, whenever a later frame gives a
//! surer plate or a closer picture, so the stamped plate is always the
//! entry's. If the scale settles heavier (the trailer rolled on after the
//! first settle), the same entry takes the new weight.
//!
//! Unlike the Electron app there is no vehicle detector: the scale is what
//! says a truck is there, the reader finds every plate in the whole frame,
//! and plate_choice picks the truck's (the biggest, toward the side the
//! settings favour) from among them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use crate::camera::{self, burst::Burst, CameraSettings};
use crate::history::{weight_value, Entry, NewEvent};
use crate::plate_choice::{self, Rules};
use crate::scale::frames::format_weight;
use crate::scale::reader::Weighing;
use crate::stamp::{self, Corner, Stamp};
use crate::truck_photos::{Photo, Truck};
use crate::{log, AppState};

/// A plate this sure, unchanged for a few frames after the truck settled,
/// is not worth more frames.
const PLATE_CONFIDENT: f64 = 0.9;
const STEADY_FRAMES: u32 = 3;
const PHOTO_TIMEOUT: Duration = Duration::from_secs(10);
const HD_TIMEOUT: Duration = Duration::from_secs(12);
/// How often the full-resolution feed writes a frame; the newest is read.
const FEED_FPS: f64 = 2.0;

#[derive(Debug)]
pub enum Event {
    Arrived,
    Weighed(Weighing),
    Reweighed(Weighing),
    Cleared,
}

/// What has been written to the history for the truck, to tell what changed.
#[derive(Default, PartialEq, Clone)]
struct Written {
    plate: Option<String>,
    /// The plate's confidence (in hundredths) and frames: rewritten on the
    /// entry as they change, but no reason to stamp the photo again.
    plate_detail: Option<(u32, u32)>,
    photo: Option<u32>,
    weight: Option<f64>,
}

/// The truck on the deck now.
#[derive(Default)]
struct Session {
    truck: Truck,
    /// Set when it settles; a reweigh changes the weight.
    weighing: Option<Weighing>,
    /// The first settled weight, once a reweigh has corrected it.
    corrected: Option<Value>,
    entry_id: Option<String>,
    written: Written,
    /// The frame loop is running.
    capturing: bool,
    /// The deck cleared, or the next truck came.
    over: bool,
    /// Why no photo, for the entry's note.
    camera_error: Option<String>,
}

type Shared = Arc<Mutex<Session>>;

#[derive(Clone)]
struct Pipeline {
    app: AppHandle,
    current: Arc<Mutex<Option<Shared>>>,
    /// One history write at a time.
    writing: Arc<tokio::sync::Mutex<()>>,
}

/// Starts the task; send the scale's events to what comes back.
pub fn start(app: AppHandle) -> mpsc::UnboundedSender<Event> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline { app, current: Arc::default(), writing: Arc::default() };
    tauri::async_runtime::spawn(async move {
        while let Some(event) = rx.recv().await {
            let p = pipeline.clone();
            match event {
                Event::Arrived => {
                    p.session(true);
                }
                Event::Weighed(w) => {
                    let session = p.session(false);
                    session.lock().unwrap().weighing = Some(w);
                    p.start_capture(&session);
                    tauri::async_runtime::spawn(async move { p.on_weighed(session).await });
                }
                Event::Reweighed(w) => {
                    let Some(session) = p.current.lock().unwrap().clone() else { continue };
                    tauri::async_runtime::spawn(async move { p.on_reweighed(session, w).await });
                }
                Event::Cleared => {
                    if let Some(session) = p.current.lock().unwrap().take() {
                        session.lock().unwrap().over = true;
                    }
                }
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

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl Pipeline {
    fn settings(&self) -> Value {
        self.app.state::<AppState>().settings.lock().unwrap().get().clone()
    }

    fn log(&self, message: impl AsRef<str>) {
        log(&self.app, message);
    }

    /// The truck on the deck, a new one if there is none. `arrived`: it is
    /// only rolling on, so the frames start now.
    fn session(&self, arrived: bool) -> Shared {
        let session = {
            let mut current = self.current.lock().unwrap();
            if let Some(s) = current.as_ref().filter(|s| !s.lock().unwrap().over) {
                return s.clone();
            }
            let fresh = Shared::default();
            *current = Some(fresh.clone());
            fresh
        };
        if arrived {
            self.log("a truck is coming onto the scale");
            self.start_capture(&session);
        }
        session
    }

    fn is_current(&self, session: &Shared) -> bool {
        self.current.lock().unwrap().as_ref().is_some_and(|c| Arc::ptr_eq(c, session))
    }

    fn stamp(&self, frame: &[u8], taken_at: DateTime<Local>, weight_text: &str, plate: Option<&str>) -> Vec<u8> {
        let s = self.settings();
        let plate_corner = Corner::named(s["plates"]["overlayCorner"].as_str().unwrap_or("top-left"));
        let weight_corner = s["scale"]["overlayCorner"].as_str().map_or(plate_corner.opposite(), Corner::named);
        let plate_text = plate.filter(|_| s["plates"]["stampPhoto"] != false);
        let stamp = Stamp {
            taken_at,
            plate: plate_text.map(|t| (t.to_string(), plate_corner)),
            weight: (s["scale"]["stampPhoto"] != false).then(|| (weight_text.to_string(), weight_corner)),
        };
        let quality = s["history"]["photoQuality"].as_u64().map_or(85, |q| q.clamp(30, 100) as u8);
        stamp::stamp_jpeg(frame, &stamp, quality).unwrap_or_else(|err| {
            self.log(format!("could not stamp the photo: {err}"));
            frame.to_vec()
        })
    }

    /// Takes frames for as long as the truck is there; once at a time.
    fn start_capture(&self, session: &Shared) {
        let s = self.settings();
        if s["scale"]["capturePhoto"] == false {
            return;
        }
        let camera: CameraSettings = serde_json::from_value(s["camera"].clone()).unwrap_or_default();
        let Some(url) = camera.url(true) else { return };
        {
            let mut session = session.lock().unwrap();
            if session.capturing || session.over {
                return;
            }
            session.capturing = true;
        }
        let (p, session) = (self.clone(), session.clone());
        tauri::async_runtime::spawn(async move {
            p.capture(&session, &url).await;
            session.lock().unwrap().capturing = false;
            // The last frames may have changed what is written.
            p.sync(&session, false).await;
        });
    }

    async fn capture(&self, session: &Shared, url: &str) {
        let s = self.settings();
        let read_plates = s["plates"]["enabled"] != false && s["scale"]["readPlate"] != false;
        let rules: Rules = serde_json::from_value(s["plates"].clone()).unwrap_or_default();
        let max_frames = s["plates"]["retryAttempts"].as_u64().filter(|&n| n > 0).unwrap_or(90) as u32;
        let every = Duration::from_secs_f64(s["plates"]["retrySeconds"].as_f64().filter(|&n| n > 0.0).unwrap_or(1.0).clamp(0.25, 30.0));
        let mut burst = Burst::start(url, FEED_FPS).map_err(|err| self.log(format!("full-resolution feed: {err}; using single snapshots"))).ok();
        let (mut taken, mut steady) = (0u32, 0u32);
        while taken < max_frames && !session.lock().unwrap().over && self.is_current(session) {
            let started = tokio::time::Instant::now();
            let frame = match burst.as_mut() {
                Some(b) => b.next(PHOTO_TIMEOUT).await.map(|(jpeg, _)| jpeg).ok_or_else(|| "the full-resolution feed sent nothing".to_string()),
                None => camera::snapshot::capture(url, "", "", HD_TIMEOUT).await,
            };
            let jpeg = match frame {
                Ok(jpeg) => jpeg,
                Err(err) => {
                    self.log(format!("no frame from the camera: {err}"));
                    session.lock().unwrap().camera_error = Some(err);
                    if burst.take().is_none() {
                        break; // snapshots fail too: the camera is not answering
                    }
                    continue;
                }
            };
            taken += 1;
            let taken_at = Local::now();
            let settled = session.lock().unwrap().weighing.is_some();
            let read = if read_plates {
                let width = camera::snapshot::jpeg_size(&jpeg).map_or(0.0, |(w, _)| f64::from(w));
                match self.app.state::<AppState>().plates.read(&jpeg).await {
                    Ok(plates) => plate_choice::choose(&plates, width, &rules).plate,
                    Err(err) => {
                        self.log(format!("plate read failed: {err}"));
                        None
                    }
                }
            } else {
                None
            };
            let seen = read.as_ref().map(|r| format!("{} {:.0}%", r.text, r.confidence * 100.0));
            let change = session.lock().unwrap().truck.add(jpeg, taken_at, read, settled);
            if change.plate || change.photo {
                let plate = session.lock().unwrap().truck.plate();
                let what = match (change.plate, &plate) {
                    (true, Some(p)) => format!("plate {} ({:.0}%, {} frame{})", p.text, p.confidence * 100.0, p.frames, if p.frames == 1 { "" } else { "s" }),
                    _ => "a closer picture".into(),
                };
                self.log(format!("frame {taken}: {what}{}", seen.map_or(String::new(), |s| format!(" - read {s}"))));
                steady = 0;
                self.sync(session, false).await;
            } else if settled {
                steady += 1;
            }
            // Settled, sure of the plate, and nothing better for a few frames.
            let sure = session.lock().unwrap().truck.plate().is_some_and(|p| p.confidence >= PLATE_CONFIDENT);
            if settled && (steady >= STEADY_FRAMES && (sure || !read_plates)) {
                break;
            }
            if let Some(rest) = every.checked_sub(started.elapsed()) {
                tokio::time::sleep(rest).await;
            }
        }
        let summary = {
            let s = session.lock().unwrap();
            let plate = s.truck.plate().map_or("no plate".to_string(), |p| format!("plate {} ({:.0}%, {} frames)", p.text, p.confidence * 100.0, p.frames));
            format!("{} frames of the truck; {plate}", s.truck.frames())
        };
        self.log(summary);
    }

    /// Settled: written now if there is a picture (or no camera), otherwise
    /// as soon as a frame comes, and without one if none comes in time.
    async fn on_weighed(&self, session: Shared) {
        let s = self.settings();
        if s["scale"]["capturePhoto"] == false {
            return;
        }
        let weighing = session.lock().unwrap().weighing.clone().expect("set before");
        self.log(format!("{} on the scale - photographing", format_weight(weighing.weight, weighing.unit)));
        let camera: CameraSettings = serde_json::from_value(s["camera"].clone()).unwrap_or_default();
        if camera.url(true).is_none() {
            self.log("weighing with no camera set up - logged without a photo");
            return self.sync(&session, true).await;
        }
        self.sync(&session, false).await;
        tokio::time::sleep(PHOTO_TIMEOUT).await;
        {
            let mut s = session.lock().unwrap();
            if s.truck.photo().is_none() && s.camera_error.is_none() {
                s.camera_error = Some(format!("no frame from the camera within {} s", PHOTO_TIMEOUT.as_secs()));
            }
        }
        self.sync(&session, true).await;
    }

    async fn on_reweighed(&self, session: Shared, weighing: Weighing) {
        let previous = weighing.previous.unwrap_or_default();
        self.log(format!(
            "{} on the scale - more of the truck; correcting {}",
            format_weight(weighing.weight, weighing.unit),
            format_weight(previous, weighing.unit)
        ));
        {
            let mut s = session.lock().unwrap();
            let at = DateTime::from_timestamp_millis(weighing.at).map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true));
            s.corrected.get_or_insert(json!({ "from": weight_value(previous), "at": at }));
            if let Some(w) = s.weighing.as_mut() {
                w.weight = weighing.weight;
                w.unit = weighing.unit;
            }
        }
        self.sync(&session, false).await;
    }

    /// Brings the history up to date with what is known of the truck: the
    /// entry written once it has settled, then rewritten - photo re-stamped -
    /// when the plate, the picture or the weight changes. `without_photo`:
    /// write it even with no picture yet.
    async fn sync(&self, session: &Shared, without_photo: bool) {
        let _one_at_a_time = self.writing.lock().await;
        let (weighing, plate, photo, entry_id, written, corrected, note) = {
            let s = session.lock().unwrap();
            let Some(weighing) = s.weighing.clone() else { return };
            let note = s.camera_error.as_ref().map(|e| format!("photo failed: {e}")).unwrap_or_default();
            (weighing, s.truck.plate(), s.truck.photo().cloned(), s.entry_id.clone(), s.written.clone(), s.corrected.clone(), note)
        };
        if entry_id.is_none() && photo.is_none() && !without_photo {
            return; // the first frame after settling is on its way
        }
        let now = Written {
            plate: plate.as_ref().map(|p| p.text.clone()),
            plate_detail: plate.as_ref().map(|p| ((p.confidence * 100.0).round() as u32, p.frames)),
            photo: photo.as_ref().map(|p| p.id),
            weight: Some(weighing.weight),
        };
        if entry_id.is_some() && now == written {
            return;
        }
        let weight_text = format_weight(weighing.weight, weighing.unit);
        let plate_json =
            plate.as_ref().map(|p| json!({ "text": p.text, "confidence": (p.confidence * 100.0).round() / 100.0, "frames": p.frames, "readAt": now_iso() }));
        // The photo is stamped again only when what is on it changes.
        let restamp = entry_id.is_none() || now.plate != written.plate || now.photo != written.photo || now.weight != written.weight;
        let stamped = photo.as_ref().filter(|_| restamp).map(|Photo { jpeg, taken_at, .. }| self.stamp(jpeg, *taken_at, &weight_text, now.plate.as_deref()));
        let state = self.app.state::<AppState>();
        let history = state.history.lock().unwrap();
        let Some(history) = history.as_ref() else { return };
        let result = match &entry_id {
            None => {
                let event = NewEvent {
                    weight: Some(weighing.weight),
                    unit: Some(weighing.unit.to_string()),
                    at: Some(weighing.at),
                    plate: plate_json.clone(),
                    note: if stamped.is_none() { note } else { String::new() },
                    ..Default::default()
                };
                // A reweigh that came before the entry was written goes on it now.
                history.record(event, stamped.as_deref()).and_then(|entry| match (&corrected, entry.get("id").and_then(Value::as_str)) {
                    (Some(corrected), Some(id)) => Ok(history.update(id, &object(json!({ "corrected": corrected })), None)?.or(Some(entry))),
                    _ => Ok(Some(entry)),
                })
            }
            Some(id) => {
                let mut patch = json!({ "weight": weight_value(weighing.weight), "unit": weighing.unit.to_string() });
                if now.plate != written.plate || now.plate_detail != written.plate_detail {
                    patch["plate"] = plate_json.clone().unwrap_or(Value::Null);
                }
                if let Some(corrected) = &corrected {
                    patch["corrected"] = corrected.clone();
                }
                if stamped.is_some() {
                    patch["photoAt"] = now_iso().into();
                    patch["note"] = "".into();
                }
                history.update(id, &object(patch), stamped.as_deref())
            }
        };
        match result {
            Ok(Some(entry)) => {
                let mut s = session.lock().unwrap();
                s.entry_id = entry.get("id").and_then(Value::as_str).map(String::from);
                s.written = now.clone();
            }
            Ok(None) => {}
            Err(err) => self.log(format!("could not write the history: {err}")),
        }
        if now.plate != written.plate {
            if let Some(plate) = plate_json {
                if let Some(scale) = state.scale.lock().unwrap().as_ref() {
                    scale.record_plate(plate.clone());
                }
                let _ = self.app.emit("scale:plate", &plate);
            }
        }
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
