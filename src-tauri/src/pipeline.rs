//! What happens when the scale weighs a truck: the weighing goes into the
//! history, and a heavier settle of the same truck corrects it.
//!
//! The scale's events arrive on a channel and are handled one at a time on
//! a task of their own, so the serial reader never waits on the disk (or,
//! later, the camera).
//!
//! Not ported yet: the photo and the plate read. Until the camera is here,
//! every weighing is logged the way the Electron app logs one when no camera
//! is set up - without a photo.

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

use crate::history::{Entry, NewEvent};
use crate::scale::reader::Weighing;
use crate::{log, AppState};

#[derive(Debug)]
pub enum Event {
    Weighed(Weighing),
    Reweighed(Weighing),
    Cleared,
}

/// Starts the task; send the scale's events to what comes back.
pub fn start(app: AppHandle) -> mpsc::UnboundedSender<Event> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tauri::async_runtime::spawn(async move {
        // The history entry of the truck on the deck now, if it has one.
        let mut current: Option<String> = None;
        while let Some(event) = rx.recv().await {
            match event {
                Event::Weighed(w) => current = on_weighed(&app, &w).and_then(|e| e.get("id")?.as_str().map(String::from)),
                Event::Reweighed(w) => {
                    if let Some(id) = &current {
                        on_reweighed(&app, id, &w);
                    }
                }
                Event::Cleared => current = None,
            }
        }
    });
    tx
}

fn on_weighed(app: &AppHandle, weighing: &Weighing) -> Option<Entry> {
    let state = app.state::<AppState>();
    let capture = state.settings.lock().unwrap().get()["scale"]["capturePhoto"].as_bool() != Some(false);
    if !capture {
        return None;
    }
    log(app, "weighing with no camera set up - logged without a photo");
    let event = NewEvent { weight: Some(weighing.weight), unit: Some(weighing.unit.to_string()), at: Some(weighing.at), ..Default::default() };
    let history = state.history.lock().unwrap();
    match history.as_ref()?.record(event, None) {
        Ok(entry) => Some(entry),
        Err(err) => {
            drop(history);
            log(app, format!("could not write the history: {err}"));
            None
        }
    }
}

/// More of the same truck on the deck: the entry keeps its id, time and
/// plate and takes the new weight.
fn on_reweighed(app: &AppHandle, id: &str, weighing: &Weighing) {
    let at = chrono::DateTime::from_timestamp_millis(weighing.at).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let patch = json!({
        "weight": crate::history::weight_value(weighing.weight),
        "unit": weighing.unit.to_string(),
        "corrected": { "from": weighing.previous.map(crate::history::weight_value), "at": at },
    });
    let Value::Object(patch) = patch else { unreachable!("json! of an object") };
    let state = app.state::<AppState>();
    let history = state.history.lock().unwrap();
    if let Some(Err(err)) = history.as_ref().map(|h| h.update(id, &patch, None)) {
        drop(history);
        log(app, format!("could not correct the history: {err}"));
    }
}
