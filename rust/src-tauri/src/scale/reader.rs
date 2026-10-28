//! The station's reader for one truck scale: a TCP connection to the serial
//! device server, the frames parsed into readings, a settled weight over the
//! threshold raised as a weighing. Status lives in memory and goes to the
//! window as events.
//!
//! Split in two. `ScaleCore` is everything that decides anything - frames in,
//! status and events out - with the clock passed in, so tests drive it with
//! made-up bytes and times. `ScaleReader` owns a background task that keeps a
//! socket open and feeds the core whatever arrives: from the device server,
//! or from another copy of the app that reads it and passes it on (share.rs).

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Instant};

use super::frames::{latin1, lenient_number, FrameSplitter, Phase, Reading, TriggerEvent, TriggerSettings, Unit, WeighingTrigger};
use super::share::{self, Decision, Share};

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(15);
const STATUS_MIN_INTERVAL_MS: i64 = 250;
/// A relay that dropped is tried again, or another, sooner than the scale.
const RELAY_RETRY: Duration = Duration::from_millis(500);

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The scale section of the settings, as far as the reader cares.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSettings {
    pub enabled: bool,
    pub host: String,
    #[serde(deserialize_with = "lenient_number")]
    pub port: f64,
    pub unit: Unit,
    /// Read the scale through another copy of the app on the LAN that reads
    /// it already, or be that copy for the others (share.rs).
    pub share: bool,
    #[serde(flatten)]
    pub trigger: TriggerSettings,
}

impl Default for ScaleSettings {
    fn default() -> Self {
        Self { enabled: false, host: String::new(), port: 950.0, unit: Unit::Lb, share: true, trigger: TriggerSettings::default() }
    }
}

/// A weighing as the window and the history see it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Weighing {
    pub weight: f64,
    pub unit: Unit,
    pub gross: bool,
    pub at: i64,
    /// The plate read off the truck, filled in later by the plate reader.
    pub plate: Option<serde_json::Value>,
    /// On a reweigh, the weight it corrects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
    /// Weighed without the scale settling (see WeighingTrigger).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    /// Pulled on before the truck ahead, weighed at this, had left: the deck
    /// never read clear between them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follows: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleStatus {
    pub connected: bool,
    /// The PC whose connection to the scale this one reads through; none
    /// when it reads the scale itself.
    pub via: Option<String>,
    pub weight: Option<f64>,
    pub unit: Unit,
    pub settled: bool,
    pub loaded: bool,
    /// empty, arriving, weighed or moving (see WeighingTrigger).
    pub phase: Phase,
    pub motion: bool,
    pub frames: u64,
    pub last_frame_at: Option<i64>,
    pub last_weighing: Option<Weighing>,
    pub last_error: String,
    pub last_error_at: Option<i64>,
    pub updated_at: i64,
}

/// What the reader tells the rest of the app.
#[derive(Debug, Clone)]
pub enum ScaleEvent {
    Status(ScaleStatus),
    /// A truck is rolling on; it has not settled yet.
    Arrived,
    /// A truck has been on the deck this long without settling, and why: the
    /// weight's spread over the settling window, and the motion flag. It
    /// carries the unit: the reader's lock is held while it is handed on, so
    /// whatever hears it must not ask the reader anything.
    Unsettled {
        seconds: i64,
        min: f64,
        max: f64,
        motion: bool,
        tolerance: f64,
        unit: Unit,
    },
    Weighed(Weighing),
    Reweighed(Weighing),
    /// The weighed truck's weight has left its band: nothing more is put
    /// down to it until the deck settles again.
    Moving {
        weight: f64,
        locked: f64,
        unit: Unit,
    },
    /// Settled again after moving, but not a new weighing (TriggerEvent::Stopped).
    Stopped {
        weight: f64,
        locked: f64,
        same: bool,
        unit: Unit,
    },
    #[allow(dead_code)] // the pipeline only needs to know the deck is clear
    Cleared {
        weight: f64,
        at: i64,
    },
    /// For the log: who reads the scale through whom.
    Note(String),
}

/// Where events go. Called with the reader's lock held, so it must not call
/// back into the reader.
pub type Sink = Arc<dyn Fn(ScaleEvent) + Send + Sync>;

/// A reading is taken to stand for at most this long when a clump of them is
/// spread back (an indicator sends a few a second; longer is a gap, not a
/// clump).
const MAX_READING_MS: i64 = 1_000;

/// When each of `n` readings arriving together at `now` was really sent. On
/// weak Wi-Fi they come a few at a time, seconds apart, and stamping them all
/// with the moment they arrived made two seconds of steady readings look like
/// an instant: a truck at a steady 31,300 lb never settled (WC-Office,
/// 2026-10-15). So a clump is spread evenly back over the time since the last
/// one, the newest at `now`. One reading at a time, as on a good link, keeps
/// its own arrival time.
pub fn spread(last: Option<i64>, now: i64, n: usize) -> Vec<i64> {
    let Some(last) = last.filter(|&l| l < now && n > 1) else { return vec![now; n] };
    let span = (now - last).min(MAX_READING_MS * n as i64);
    (1..=n as i64).map(|i| now - span + span * i / n as i64).collect()
}

const UNSETTLED_AFTER_MS: i64 = 15_000;
/// How far back the readings put in the log go.
const TRACE_SECONDS: i64 = 30;
const UNSETTLED_EVERY_MS: i64 = 30_000;

pub struct ScaleCore {
    unit: Unit,
    splitter: FrameSplitter,
    trigger: WeighingTrigger,
    status: ScaleStatus,
    last_emit_at: i64,
    /// When the last readings came in, to spread a clump of them back over
    /// the time they were really sent in (see `spread`).
    last_chunk_at: Option<i64>,
    /// When the truck on the deck arrived, and when its not settling was last said.
    arrived_at: Option<i64>,
    unsettled_said_at: Option<i64>,
    sink: Sink,
}

impl ScaleCore {
    pub fn new(settings: &ScaleSettings, sink: Sink, now: i64) -> Self {
        Self {
            unit: settings.unit,
            splitter: FrameSplitter::new(true),
            trigger: WeighingTrigger::new(&settings.trigger),
            status: ScaleStatus {
                connected: false,
                via: None,
                weight: None,
                unit: settings.unit,
                settled: false,
                loaded: false,
                phase: Phase::Empty,
                motion: false,
                frames: 0,
                last_frame_at: None,
                last_weighing: None,
                last_error: String::new(),
                last_error_at: None,
                updated_at: now,
            },
            last_emit_at: 0,
            last_chunk_at: None,
            arrived_at: None,
            unsettled_said_at: None,
            sink,
        }
    }

    /// Settings were saved while a truck sat on the deck: the reader is made
    /// afresh, but the truck is the same one, already weighed. Carry that
    /// over so it is not weighed again two seconds later.
    pub fn adopt(&mut self, previous: &ScaleCore) {
        self.trigger.armed = previous.trigger.armed;
        self.trigger.last_weighing = previous.trigger.last_weighing;
        self.status.last_weighing = previous.status.last_weighing.clone();
        self.status.loaded = !self.trigger.armed;
        self.status.phase = self.trigger.phase();
    }

    pub fn status(&self) -> &ScaleStatus {
        &self.status
    }

    pub fn loaded(&self) -> bool {
        !self.trigger.armed
    }

    /// Twenty frames a second is more than a window needs; the status goes
    /// out at most four times a second unless something notable happened.
    fn set_status(&mut self, now: i64, force: bool, patch: impl FnOnce(&mut ScaleStatus)) {
        patch(&mut self.status);
        self.status.updated_at = now;
        if !force && now - self.last_emit_at < STATUS_MIN_INTERVAL_MS {
            return;
        }
        self.last_emit_at = now;
        (self.sink)(ScaleEvent::Status(self.status.clone()));
    }

    pub fn connecting(&mut self, via: Option<String>) {
        self.splitter = FrameSplitter::new(true);
        self.status.via = via;
    }

    pub fn connected(&mut self, now: i64) {
        self.set_status(now, true, |s| {
            s.connected = true;
            s.last_error.clear();
        });
    }

    pub fn disconnected(&mut self, now: i64) {
        self.set_status(now, true, |s| {
            s.connected = false;
            s.settled = false;
        });
    }

    pub fn error(&mut self, message: impl Into<String>, now: i64) {
        let message = message.into();
        self.set_status(now, true, |s| {
            s.last_error = message;
            s.last_error_at = Some(now);
        });
    }

    pub fn on_data(&mut self, chunk: &[u8], now: i64) {
        let readings = self.splitter.feed(chunk);
        let times = spread(self.last_chunk_at, now, readings.len());
        if !readings.is_empty() {
            self.last_chunk_at = Some(now);
        }
        for (reading, at) in readings.into_iter().zip(times) {
            let loaded_before = !self.trigger.armed;
            let event = self.trigger.push(&reading, at);
            let (settled, loaded, unit) = (self.trigger.settled, !self.trigger.armed, reading.unit.unwrap_or(self.unit));
            let phase = self.trigger.phase();
            if phase != self.status.phase {
                (self.sink)(ScaleEvent::Note(format!(
                    "scale: {} -> {} at {}",
                    self.status.phase.name(),
                    phase.name(),
                    super::frames::format_weight(reading.weight, unit)
                )));
            }
            self.set_status(now, phase != self.status.phase, |s| {
                s.weight = Some(reading.weight);
                s.unit = unit;
                s.settled = settled;
                s.loaded = loaded;
                s.phase = phase;
                s.motion = reading.motion;
                s.frames += 1;
                s.last_frame_at = Some(now);
            });
            match event {
                None => {}
                Some(TriggerEvent::Arrived { .. }) => {
                    self.arrived_at = Some(now);
                    (self.sink)(ScaleEvent::Arrived)
                }
                Some(TriggerEvent::Weighed { weight, unit, gross, estimated, follows, .. }) => {
                    self.arrived_at = None;
                    if follows.is_some() {
                        self.trace(now);
                    }
                    let weighing = Weighing { weight, unit: unit.unwrap_or(self.unit), gross, at: now, plate: None, previous: None, estimated, follows };
                    self.set_status(now, true, |s| s.last_weighing = Some(weighing.clone()));
                    (self.sink)(ScaleEvent::Weighed(weighing));
                }
                Some(TriggerEvent::Reweighed { weight, previous, unit, gross, .. }) => {
                    // More of the same truck on the deck: the weighing is
                    // corrected, its plate kept.
                    self.arrived_at = None;
                    self.trace(now);
                    let plate = self.status.last_weighing.as_ref().and_then(|w| w.plate.clone());
                    let weighing =
                        Weighing { weight, unit: unit.unwrap_or(self.unit), gross, at: now, plate, previous: Some(previous), estimated: false, follows: None };
                    self.set_status(now, true, |s| s.last_weighing = Some(weighing.clone()));
                    (self.sink)(ScaleEvent::Reweighed(weighing));
                }
                Some(TriggerEvent::Moving { weight, locked, .. }) => {
                    // Counted as arriving afresh, for saying it has not settled.
                    self.arrived_at = Some(now);
                    self.trace(now);
                    (self.sink)(ScaleEvent::Moving { weight, locked, unit });
                }
                Some(TriggerEvent::Stopped { weight, locked, same, .. }) => {
                    if same {
                        self.arrived_at = None;
                    }
                    self.trace(now);
                    (self.sink)(ScaleEvent::Stopped { weight, locked, same, unit });
                }
                Some(TriggerEvent::Cleared { weight, .. }) => {
                    self.arrived_at = None;
                    if loaded_before {
                        self.trace(now);
                    }
                    self.set_status(now, true, |s| s.loaded = false);
                    (self.sink)(ScaleEvent::Cleared { weight, at: now });
                }
            }
            self.say_if_unsettled(now);
        }
    }

    /// What the deck read over the last half minute, into the log: when a
    /// truck is moving, settles again, or a new one is weighed behind it, a
    /// bug report then shows exactly what the scale did.
    fn trace(&self, now: i64) {
        let trace = self.trigger.trace(now, TRACE_SECONDS);
        if !trace.is_empty() {
            (self.sink)(ScaleEvent::Note(format!("scale readings, last {TRACE_SECONDS} s, every 0.5 s (m: motion flag): {trace}")));
        }
    }

    /// A truck on the deck that has not settled after UNSETTLED_AFTER_MS is
    /// said, with why, every UNSETTLED_EVERY_MS: a truck that never settles
    /// is never weighed (WC-Office, 2026-10-14 07:58, eight minutes on the
    /// deck), and the log has to show whether it was the weight wandering or
    /// the motion flag.
    fn say_if_unsettled(&mut self, now: i64) {
        let Some(arrived_at) = self.arrived_at else { return };
        let said_lately = self.unsettled_said_at.is_some_and(|at| at >= arrived_at && now - at < UNSETTLED_EVERY_MS);
        let waiting = self.trigger.armed || self.trigger.phase() == Phase::Moving;
        if !waiting || now - arrived_at < UNSETTLED_AFTER_MS || said_lately {
            return;
        }
        if let Some((min, max, motion, tolerance)) = self.trigger.window_spread() {
            self.unsettled_said_at = Some(now);
            (self.sink)(ScaleEvent::Unsettled { seconds: (now - arrived_at) / 1000, min, max, motion, tolerance, unit: self.status.unit });
        }
    }

    /// The plate read off the weighed truck, kept with the weighing so the
    /// overlay can show it while the truck is still on the deck.
    #[allow(dead_code)] // the plate reader is not ported yet
    pub fn record_plate(&mut self, plate: serde_json::Value, now: i64) {
        if self.status.last_weighing.is_none() {
            return;
        }
        self.set_status(now, true, |s| {
            if let Some(w) = s.last_weighing.as_mut() {
                w.plate = Some(plate);
            }
        });
    }

    #[cfg(test)]
    pub fn last_weighing(&self) -> Option<super::frames::LastWeighing> {
        self.trigger.last_weighing
    }
}

/// A running reader: the core, and the task keeping its socket open.
pub struct ScaleReader {
    core: Arc<Mutex<ScaleCore>>,
    task: JoinHandle<()>,
    share: Option<Arc<Share>>,
}

impl ScaleReader {
    /// Starts reading, carrying over the truck on the deck from `previous`.
    pub fn start(settings: &ScaleSettings, previous: Option<&ScaleReader>, sink: Sink) -> Self {
        let share = settings.share.then(|| Share::new(share::scale_key(&settings.host, port_of(settings.port)), share::Net::default(), sink.clone()));
        Self::start_shared(settings, previous, sink, share)
    }

    pub fn start_shared(settings: &ScaleSettings, previous: Option<&ScaleReader>, sink: Sink, share: Option<Arc<Share>>) -> Self {
        let mut core = ScaleCore::new(settings, sink, now_ms());
        if let Some(previous) = previous {
            core.adopt(&previous.lock());
        }
        let core = Arc::new(Mutex::new(core));
        let task = tauri::async_runtime::spawn(run(core.clone(), settings.host.trim().to_string(), port_of(settings.port), share.clone()));
        Self { core, task, share }
    }

    fn lock(&self) -> MutexGuard<'_, ScaleCore> {
        // A panic while holding the lock leaves the core as it was; carry on.
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> ScaleStatus {
        self.lock().status().clone()
    }

    /// The status if it can be had without waiting: for the crash reports,
    /// which must not wait on a reader that may be stuck.
    pub fn try_status(&self) -> Option<ScaleStatus> {
        self.core.try_lock().ok().map(|c| c.status().clone())
    }

    /// Whether the reader is free now. It holds its lock only for a moment
    /// per reading; held for long, it is stuck (the watchdog asks).
    pub fn answers(&self) -> bool {
        self.core.try_lock().is_ok()
    }

    #[allow(dead_code)] // the weighing pipeline is not ported yet
    pub fn loaded(&self) -> bool {
        self.lock().loaded()
    }

    #[allow(dead_code)] // the plate reader is not ported yet
    pub fn record_plate(&self, plate: serde_json::Value) {
        self.lock().record_plate(plate, now_ms());
    }

    pub fn stop(&self) {
        self.task.abort();
        if let Some(share) = &self.share {
            share.bye();
        }
        self.lock().disconnected(now_ms());
    }
}

impl Drop for ScaleReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn port_of(port: f64) -> u16 {
    if (1.0..=65535.0).contains(&port) {
        port as u16
    } else {
        950
    }
}

async fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let stream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Connection timed out"))??;
    stream.set_nodelay(true)?;
    let keepalive = socket2::TcpKeepalive::new().with_time(Duration::from_secs(10));
    socket2::SockRef::from(&stream).set_tcp_keepalive(&keepalive)?;
    Ok(stream)
}

/// Connect, read until the line drops or goes quiet, wait, and again, for as
/// long as the reader runs. The wait doubles each time, up to a minute, and
/// starts over once a connection is made.
///
/// When sharing, where it connects is the election's to say: the scale, with
/// what it reads passed on to the copies that follow, or the leader's relay.
/// It moves the moment that changes.
async fn run(core: Arc<Mutex<ScaleCore>>, host: String, port: u16, share: Option<Arc<Share>>) {
    let lock = || core.lock().unwrap_or_else(|e| e.into_inner());
    let sink = lock().sink.clone();
    let note = |message: &str| sink(ScaleEvent::Note(message.to_string()));
    let _sharing = match &share {
        Some(share) => Some(share.start().await),
        None => None,
    };
    let mut decisions = share.as_ref().map(|s| s.decisions());
    let mut relaying = None;
    let mut backoff = RECONNECT_MIN;
    loop {
        let source = match decisions.as_mut() {
            Some(decisions) => share::next(decisions).await,
            None => Decision::Lead,
        };
        let (target, via, leader) = match &source {
            Decision::Follow { id, addr, name } => ((addr.ip().to_string(), addr.port()), Some(name.clone()), Some(*id)),
            _ => ((host.clone(), port), None, None),
        };
        match (&source, &share) {
            (Decision::Lead, Some(share)) if relaying.is_none() => {
                relaying = Some(share.lead());
                note("reading the scale directly, and passing it on to any other PC on the network with this app open");
            }
            (Decision::Follow { name, .. }, _) => {
                if relaying.take().is_some() {
                    note("an older copy of the app on another PC reads the scale too: handing over to it");
                }
                if lock().status().via.as_ref() != Some(name) {
                    note(&format!("reading the scale through {name}, which passes it on"));
                }
            }
            _ => {}
        }
        lock().connecting(via);
        let mut connected = false;
        match connect(&target.0, target.1).await {
            Ok(mut stream) => {
                connected = true;
                if let (Some(share), Some(id)) = (&share, leader) {
                    share.reached(id);
                }
                backoff = RECONNECT_MIN;
                lock().connected(now_ms());
                let mut buf = [0u8; 4096];
                loop {
                    let read = tokio::select! {
                        read = timeout(SILENCE_TIMEOUT, stream.read(&mut buf)) => read,
                        _ = share::moved(&mut decisions, &source) => break,
                    };
                    match read {
                        Err(_) => {
                            lock().error("No data from the scale", now_ms());
                            break;
                        }
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            lock().on_data(&buf[..n], now_ms());
                            if let Some(relaying) = &relaying {
                                relaying.pass_on(&buf[..n]);
                            }
                        }
                        Ok(Err(err)) => {
                            lock().error(err.to_string(), now_ms());
                            break;
                        }
                    }
                }
            }
            Err(err) => lock().error(err.to_string(), now_ms()),
        }
        lock().disconnected(now_ms());
        let wait = match (&share, leader) {
            (Some(share), Some(id)) => {
                share.lost(id, connected);
                RELAY_RETRY
            }
            _ => {
                let wait = backoff;
                backoff = (backoff * 2).min(RECONNECT_MAX);
                wait
            }
        };
        tokio::select! {
            _ = sleep(wait) => {}
            _ = share::moved(&mut decisions, &source) => {}
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TestArgs {
    pub host: String,
    #[serde(deserialize_with = "lenient_number")]
    pub port: f64,
    #[serde(deserialize_with = "lenient_number")]
    pub seconds: f64,
}

impl Default for TestArgs {
    fn default() -> Self {
        Self { host: String::new(), port: 950.0, seconds: 3.0 }
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub connected: bool,
    pub error: String,
    pub frames: usize,
    pub duration_ms: u128,
    pub frames_per_second: u64,
    pub latest: Option<Reading>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub sample: String,
}

/// For the Scale tab: listen for a few seconds with the address as typed.
pub async fn test_connection(args: TestArgs) -> TestResult {
    let host = args.host.trim();
    if host.is_empty() {
        return TestResult { error: "Enter the device server address first".into(), ..Default::default() };
    }
    let started = Instant::now();
    let seconds = if args.seconds == 0.0 { 3.0 } else { args.seconds.clamp(1.0, 10.0) };
    let deadline = started + Duration::from_secs_f64(seconds);

    let mut splitter = FrameSplitter::new(true);
    let mut readings: Vec<Reading> = Vec::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut error = String::new();
    let mut connected = false;

    match connect(host, port_of(args.port)).await {
        Err(err) => error = err.to_string(),
        Ok(mut stream) => {
            connected = true;
            let mut buf = [0u8; 4096];
            while readings.len() < 200 {
                match tokio::time::timeout_at(deadline, stream.read(&mut buf)).await {
                    Err(_) | Ok(Ok(0)) => break,
                    Ok(Ok(n)) => {
                        if raw.len() < 120 {
                            raw.extend_from_slice(&buf[..n]);
                        }
                        readings.extend(splitter.feed(&buf[..n]));
                    }
                    Ok(Err(err)) => {
                        error = err.to_string();
                        break;
                    }
                }
            }
        }
    }

    let elapsed = started.elapsed();
    if connected && error.is_empty() && readings.is_empty() {
        error = "Connected, but nothing arrived. Is this the data port, and is the indicator in continuous output mode?".into();
    }
    let weights = readings.iter().map(|r| r.weight);
    // Control characters written out, so <02> and <0d><0a> show what framing the indicator uses.
    let sample = latin1(&raw[..raw.len().min(120)]).chars().map(|c| if (c as u32) < 0x20 { format!("<{:02x}>", c as u32) } else { c.to_string() }).collect();
    TestResult {
        ok: connected && error.is_empty() && !readings.is_empty(),
        connected,
        frames: readings.len(),
        duration_ms: elapsed.as_millis(),
        frames_per_second: if readings.is_empty() { 0 } else { (readings.len() as f64 / elapsed.as_secs_f64().max(0.5)).round() as u64 },
        min: weights.clone().reduce(f64::min),
        max: weights.reduce(f64::max),
        latest: readings.pop(),
        error,
        sample,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(weight: u32) -> Vec<u8> {
        format!("\x02{weight:>7}LG \r\n").into_bytes()
    }

    fn settings() -> ScaleSettings {
        serde_json::from_value(serde_json::json!({
            "host": "127.0.0.1", "port": 1, "unit": "lb", "minWeight": 4000, "clearWeight": 1000, "stableSeconds": 2, "stableTolerance": 40
        }))
        .unwrap()
    }

    /// A sink that keeps the weights of the weighings it is handed.
    fn recording() -> (Sink, Arc<Mutex<Vec<f64>>>) {
        let weighed = Arc::new(Mutex::new(Vec::new()));
        let w = weighed.clone();
        let sink: Sink = Arc::new(move |e| {
            if let ScaleEvent::Weighed(x) = e {
                w.lock().unwrap().push(x.weight);
            }
        });
        (sink, weighed)
    }

    // Ported from tests/scaleReader.test.js.
    #[test]
    fn a_clump_is_spread_back_over_the_time_since_the_last() {
        assert_eq!(spread(Some(1000), 3400, 6), [1400, 1800, 2200, 2600, 3000, 3400]);
        assert_eq!(spread(Some(1000), 1400, 1), [1400]); // one at a time: as it came
        assert_eq!(spread(None, 5000, 3), [5000, 5000, 5000]); // the first clump: nothing to go by
        assert_eq!(spread(Some(0), 60_000, 2), [59_000, 60_000]); // a long gap is a gap, not a clump
    }

    #[test]
    fn readings_that_arrive_in_clumps_still_settle() {
        // WC-Office, 2026-10-15: the readings reached it a few at a time,
        // seconds apart, and a truck reading 31,300 lb with no motion flag
        // was still "not settled" after 21 s. Here: 2.5 readings a second,
        // delivered in clumps of six every 2.4 s.
        let (sink, weighed) = recording();
        let mut core = ScaleCore::new(&settings(), sink, 0);
        let mut now = 0;
        let mut clump = |core: &mut ScaleCore, weight: u32, now: &mut i64| {
            let bytes: Vec<u8> = (0..6).flat_map(|_| frame(weight)).collect();
            *now += 2400;
            core.on_data(&bytes, *now);
        };
        for _ in 0..3 {
            clump(&mut core, 0, &mut now);
        }
        for _ in 0..4 {
            clump(&mut core, 31_300, &mut now);
        }
        assert_eq!(*weighed.lock().unwrap(), [31_300.0], "settled on clumped readings");
    }

    #[test]
    fn says_why_a_truck_on_the_deck_has_not_settled() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let sink: Sink = Arc::new(move |e| seen.lock().unwrap().push(e));
        let mut core = ScaleCore::new(&settings(), sink, 0);
        let frame = |w: i64| format!("\x02{w:>7}LG \r\n");
        let mut now = 0;
        for _ in 0..10 {
            core.on_data(frame(0).as_bytes(), now);
            now += 250;
        }
        // On, wandering 250 lb either way: never within the 40 lb it settles
        // to, nor the 200 lb it is weighed anyway within after 20 s.
        for i in 0..200 {
            core.on_data(frame(35_560 + if i % 2 == 0 { 250 } else { -250 }).as_bytes(), now);
            now += 250;
        }
        let said: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let ScaleEvent::Unsettled { seconds, min, max, motion, .. } = e { Some((*seconds, *min, *max, *motion)) } else { None })
            .collect();
        assert_eq!(said.len(), 2, "once at 15 s and again 30 s later: {said:?}");
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, ScaleEvent::Unsettled { unit: Unit::Lb, .. })));
        assert_eq!((said[0].1, said[0].2, said[0].3), (35_310.0, 35_810.0, false));
        assert!(said[0].0 >= 15 && said[1].0 >= 45);
    }

    #[test]
    fn tells_the_log_how_a_truck_pulling_on_behind_another_went() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let sink: Sink = Arc::new(move |e| seen.lock().unwrap().push(e));
        let mut core = ScaleCore::new(&settings(), sink, 0);
        let mut now = 0;
        let mut feed = |core: &mut ScaleCore, n: usize, w: u32| {
            for _ in 0..n {
                core.on_data(&frame(w), now);
                now += 100;
            }
        };
        feed(&mut core, 10, 0);
        feed(&mut core, 30, 32_000); // weighed
        assert_eq!(core.status().phase, Phase::Weighed);
        feed(&mut core, 10, 20_000); // driving off
        assert_eq!(core.status().phase, Phase::Moving);
        feed(&mut core, 30, 27_500); // the next truck
        let events = events.lock().unwrap();
        let weighings: Vec<_> = events.iter().filter_map(|e| if let ScaleEvent::Weighed(w) = e { Some((w.weight, w.follows)) } else { None }).collect();
        assert_eq!(weighings, [(32_000.0, None), (27_500.0, Some(32_000.0))]);
        assert!(events.iter().any(|e| matches!(e, ScaleEvent::Moving { locked: 32_000.0, weight: 20_000.0, .. })));
        let notes: Vec<&str> = events.iter().filter_map(|e| if let ScaleEvent::Note(n) = e { Some(n.as_str()) } else { None }).collect();
        for change in ["empty -> arriving", "arriving -> weighed", "weighed -> moving", "moving -> weighed"] {
            assert!(notes.iter().any(|n| n.starts_with(&format!("scale: {change}"))), "{change} in {notes:?}");
        }
        assert!(notes.iter().any(|n| n.starts_with("scale readings, last 30 s") && n.contains("32000 32000") && n.contains("20000")), "{notes:?}");
        assert_eq!(core.status().last_weighing.as_ref().unwrap().follows, Some(32_000.0));
    }

    #[test]
    fn a_reader_that_adopts_a_loaded_one_does_not_weigh_the_same_truck_again() {
        let (sink, weighed) = recording();
        let mut clock = 0;
        let mut old = ScaleCore::new(&settings(), sink.clone(), clock);
        for _ in 0..50 {
            clock += 50;
            old.on_data(&frame(32000), clock);
        }
        assert_eq!(*weighed.lock().unwrap(), [32000.0]);
        assert!(old.loaded());

        let mut fresh = ScaleCore::new(&settings(), sink, clock);
        fresh.adopt(&old);
        assert!(fresh.loaded());
        assert_eq!(fresh.status().last_weighing.as_ref().unwrap().weight, 32000.0);
        assert_eq!(fresh.last_weighing().unwrap().weight, 32000.0);
        for _ in 0..100 {
            clock += 50;
            fresh.on_data(&frame(32000), clock);
        }
        assert_eq!(*weighed.lock().unwrap(), [32000.0]);
        for weight in [0, 41000] {
            for _ in 0..50 {
                clock += 50;
                fresh.on_data(&frame(weight), clock);
            }
        }
        assert_eq!(*weighed.lock().unwrap(), [32000.0, 41000.0]);
    }

    #[test]
    fn status_goes_out_at_most_four_times_a_second() {
        let statuses = Arc::new(Mutex::new(0));
        let s = statuses.clone();
        let sink: Sink = Arc::new(move |e| {
            if let ScaleEvent::Status(_) = e {
                *s.lock().unwrap() += 1;
            }
        });
        let mut core = ScaleCore::new(&settings(), sink, 0);
        for i in 0..20 {
            core.on_data(&frame(140), 1000 + i * 50); // one second of frames
        }
        assert_eq!(core.status().frames, 19); // the first is dropped to resync
        assert_eq!(*statuses.lock().unwrap(), 4);
    }

    // Against a fake indicator on a local socket: the real connect and read.
    #[tokio::test]
    async fn the_test_button_reads_a_live_stream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut socket, _) = listener.accept().await.unwrap();
            for weight in [140, 140, 150, 48320] {
                socket.write_all(&frame(weight)).await.unwrap();
            }
        });
        let r = test_connection(TestArgs { host: "127.0.0.1".into(), port: port as f64, seconds: 1.0 }).await;
        assert!(r.ok, "{r:?}");
        assert_eq!(r.frames, 3);
        assert_eq!((r.min, r.max), (Some(140.0), Some(48320.0)));
        assert!(r.sample.starts_with("<02>    140LG <0d><0a>"));
    }

    #[tokio::test]
    async fn the_test_button_says_when_nothing_is_listening() {
        let r = test_connection(TestArgs { host: "127.0.0.1".into(), port: 1.0, seconds: 1.0 }).await;
        assert!(!r.ok && !r.connected && !r.error.is_empty(), "{r:?}");
    }
}
