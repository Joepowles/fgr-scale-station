//! The station's reader for one truck scale: a TCP connection to the serial
//! device server, the frames parsed into readings, a settled weight over the
//! threshold raised as a weighing. Status lives in memory and goes to the
//! window as events.
//!
//! Split in two. `ScaleCore` is everything that decides anything - frames in,
//! status and events out - with the clock passed in, so tests drive it with
//! made-up bytes and times. `ScaleReader` owns a background task that keeps a
//! socket open and feeds the core whatever arrives.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Instant};

use super::frames::{latin1, lenient_number, FrameSplitter, Reading, TriggerEvent, TriggerSettings, Unit, WeighingTrigger};

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(15);
const STATUS_MIN_INTERVAL_MS: i64 = 250;

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
    #[serde(flatten)]
    pub trigger: TriggerSettings,
}

impl Default for ScaleSettings {
    fn default() -> Self {
        Self { enabled: false, host: String::new(), port: 950.0, unit: Unit::Lb, trigger: TriggerSettings::default() }
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
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScaleStatus {
    pub connected: bool,
    pub weight: Option<f64>,
    pub unit: Unit,
    pub settled: bool,
    pub loaded: bool,
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
    Weighed(Weighing),
    Reweighed(Weighing),
    #[allow(dead_code)] // read by the weighing pipeline, not ported yet
    Cleared {
        weight: f64,
        at: i64,
    },
}

/// Where events go. Called with the reader's lock held, so it must not call
/// back into the reader.
pub type Sink = Arc<dyn Fn(ScaleEvent) + Send + Sync>;

pub struct ScaleCore {
    unit: Unit,
    splitter: FrameSplitter,
    trigger: WeighingTrigger,
    status: ScaleStatus,
    last_emit_at: i64,
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
                weight: None,
                unit: settings.unit,
                settled: false,
                loaded: false,
                motion: false,
                frames: 0,
                last_frame_at: None,
                last_weighing: None,
                last_error: String::new(),
                last_error_at: None,
                updated_at: now,
            },
            last_emit_at: 0,
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

    pub fn connecting(&mut self) {
        self.splitter = FrameSplitter::new(true);
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
        for reading in self.splitter.feed(chunk) {
            let event = self.trigger.push(&reading, now);
            let (settled, loaded, unit) = (self.trigger.settled, !self.trigger.armed, reading.unit.unwrap_or(self.unit));
            self.set_status(now, false, |s| {
                s.weight = Some(reading.weight);
                s.unit = unit;
                s.settled = settled;
                s.loaded = loaded;
                s.motion = reading.motion;
                s.frames += 1;
                s.last_frame_at = Some(now);
            });
            match event {
                None => {}
                Some(TriggerEvent::Weighed { weight, unit, gross, .. }) => {
                    let weighing = Weighing { weight, unit: unit.unwrap_or(self.unit), gross, at: now, plate: None, previous: None };
                    self.set_status(now, true, |s| s.last_weighing = Some(weighing.clone()));
                    (self.sink)(ScaleEvent::Weighed(weighing));
                }
                Some(TriggerEvent::Reweighed { weight, previous, unit, gross, .. }) => {
                    // More of the same truck on the deck: the weighing is
                    // corrected, its plate kept.
                    let plate = self.status.last_weighing.as_ref().and_then(|w| w.plate.clone());
                    let weighing = Weighing { weight, unit: unit.unwrap_or(self.unit), gross, at: now, plate, previous: Some(previous) };
                    self.set_status(now, true, |s| s.last_weighing = Some(weighing.clone()));
                    (self.sink)(ScaleEvent::Reweighed(weighing));
                }
                Some(TriggerEvent::Cleared { weight, .. }) => {
                    self.set_status(now, true, |s| s.loaded = false);
                    (self.sink)(ScaleEvent::Cleared { weight, at: now });
                }
            }
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
}

impl ScaleReader {
    /// Starts reading, carrying over the truck on the deck from `previous`.
    pub fn start(settings: &ScaleSettings, previous: Option<&ScaleReader>, sink: Sink) -> Self {
        let mut core = ScaleCore::new(settings, sink, now_ms());
        if let Some(previous) = previous {
            core.adopt(&previous.lock());
        }
        let core = Arc::new(Mutex::new(core));
        let task = tauri::async_runtime::spawn(run(core.clone(), settings.host.trim().to_string(), port_of(settings.port)));
        Self { core, task }
    }

    fn lock(&self) -> MutexGuard<'_, ScaleCore> {
        // A panic while holding the lock leaves the core as it was; carry on.
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> ScaleStatus {
        self.lock().status().clone()
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
async fn run(core: Arc<Mutex<ScaleCore>>, host: String, port: u16) {
    let lock = || core.lock().unwrap_or_else(|e| e.into_inner());
    let mut backoff = RECONNECT_MIN;
    loop {
        lock().connecting();
        match connect(&host, port).await {
            Ok(mut stream) => {
                backoff = RECONNECT_MIN;
                lock().connected(now_ms());
                let mut buf = [0u8; 4096];
                loop {
                    match timeout(SILENCE_TIMEOUT, stream.read(&mut buf)).await {
                        Err(_) => {
                            lock().error("No data from the scale", now_ms());
                            break;
                        }
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => lock().on_data(&buf[..n], now_ms()),
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
        sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_MAX);
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
