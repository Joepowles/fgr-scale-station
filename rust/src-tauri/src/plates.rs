//! Licence plate reading, done by a helper kept running in the background:
//! plate-reader(.exe), built from plate-reader/ in the repository, which
//! runs fast-alpr's two open models (a YOLO that finds plates and a
//! character reader for the crop) on ONNX Runtime. The Electron app drives
//! the same helper.
//!
//! The helper speaks JSON lines: one request per line on its stdin
//! ({"id": 1, "path": "frame.jpg"}), one reply per line on its stdout. It is
//! started once and kept, because loading the models is most of the cost.
//! When the app exits the helper's stdin closes, and it exits too.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;
use tokio::time::{timeout, Instant};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RESTART_BACKOFF: Duration = Duration::from_secs(30);
// Noise from onnxruntime inside an LXC container: it tries to pin its
// threads to cores and the container forbids it. Harmless, and it says so
// eight times per model.
const IGNORED_STDERR: [&str; 1] = ["pthread_setaffinity_np"];

/// One plate found in a picture. The box is in pixels of the picture given.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlateRead {
    pub text: String,
    /// The weakest character's confidence: one wrong digit is a wrong plate.
    pub confidence: f64,
    #[serde(default)]
    pub char_confidence: Vec<f64>,
    #[serde(default)]
    pub det_confidence: f64,
    #[serde(default, rename = "box")]
    pub bounds: [i64; 4],
}

#[derive(Debug, Deserialize)]
struct Reply {
    id: Option<u64>,
    ready: Option<bool>,
    #[serde(default)]
    plates: Vec<PlateRead>,
    error: Option<String>,
}

/// Where the helper and its models are.
#[derive(Debug, Clone)]
pub struct Paths {
    pub exe: PathBuf,
    pub models: PathBuf,
}

impl Paths {
    /// The installed layout under the app's resources: bin/plate-reader(.exe)
    /// and plate-models/. PLATE_READER_EXE and PLATE_READER_MODELS point at
    /// another build, for development (see the README).
    pub fn under(resources: &std::path::Path) -> Self {
        let exe = if cfg!(windows) { "plate-reader.exe" } else { "plate-reader" };
        Self {
            exe: std::env::var_os("PLATE_READER_EXE").map(PathBuf::from).unwrap_or_else(|| resources.join("bin").join(exe)),
            models: std::env::var_os("PLATE_READER_MODELS").map(PathBuf::from).unwrap_or_else(|| resources.join("plate-models")),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub installed: bool,
    pub reader: String,
    pub running: bool,
    pub last_error: String,
}

type Waiting = oneshot::Sender<Result<Vec<PlateRead>, String>>;

/// A running helper.
struct Worker {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, Waiting>>>,
    next_id: AtomicU64,
    /// Held so the process lives as long as this does (kill_on_drop).
    _child: Child,
}

#[derive(Default)]
struct Health {
    last_error: String,
    last_failure: Option<Instant>,
    /// Set by the stdout task when the helper exits.
    exited: bool,
}

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

pub struct PlateReader {
    paths: Paths,
    log: Log,
    /// An async mutex: starting the helper takes seconds, and a second read
    /// arriving meanwhile should wait for that one rather than start another.
    worker: tokio::sync::Mutex<Option<Arc<Worker>>>,
    health: Arc<Mutex<Health>>,
}

impl PlateReader {
    pub fn new(paths: Paths, log: Log) -> Self {
        Self { paths, log, worker: tokio::sync::Mutex::new(None), health: Arc::default() }
    }

    pub fn installed(&self) -> bool {
        self.paths.exe.is_file() && self.paths.models.is_dir()
    }

    pub fn status(&self) -> Status {
        let health = self.health.lock().unwrap();
        // Locked means starting, which is not running yet.
        let running = self.worker.try_lock().map(|w| w.is_some() && !health.exited).unwrap_or(false);
        Status { installed: self.installed(), reader: self.paths.exe.display().to_string(), running, last_error: health.last_error.clone() }
    }

    fn fail(&self, message: String) -> String {
        let mut health = self.health.lock().unwrap();
        health.last_error = message.clone();
        health.last_failure = Some(Instant::now());
        message
    }

    /// The running helper, started if need be.
    async fn worker(&self) -> Result<Arc<Worker>, String> {
        let mut slot = self.worker.lock().await;
        if let Some(worker) = slot.as_ref() {
            if !self.health.lock().unwrap().exited {
                return Ok(worker.clone());
            }
            *slot = None; // exited; dropping it reaps the process
        }
        if !self.installed() {
            return Err(self.fail(format!("plate reader not installed (nothing at {})", self.paths.exe.display())));
        }
        {
            let health = self.health.lock().unwrap();
            if health.last_failure.is_some_and(|at| at.elapsed() < RESTART_BACKOFF) {
                return Err(if health.last_error.is_empty() {
                    "plate reader recently failed; waiting before retrying".into()
                } else {
                    health.last_error.clone()
                });
            }
        }
        let worker = Arc::new(self.start().await.map_err(|e| self.fail(e))?);
        self.health.lock().unwrap().last_error.clear();
        *slot = Some(worker.clone());
        Ok(worker)
    }

    async fn start(&self) -> Result<Worker, String> {
        let started = Instant::now();
        let mut cmd = Command::new(&self.paths.exe);
        cmd.arg("--models").arg(&self.paths.models).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|e| format!("plate reader could not be started: {e}"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");

        let log = self.log.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if !line.is_empty() && !IGNORED_STDERR.iter().any(|noise| line.contains(noise)) {
                    log(format!("plate reader: {}", line.chars().take(300).collect::<String>()));
                }
            }
        });

        // The first line says whether the models loaded; every line after is
        // the answer to a request, matched up by id.
        let pending: Arc<Mutex<HashMap<u64, Waiting>>> = Arc::default();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
        let (waiting, health, log) = (pending.clone(), self.health.clone(), self.log.clone());
        self.health.lock().unwrap().exited = false;
        tauri::async_runtime::spawn(async move {
            let mut ready_tx = Some(ready_tx);
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(reply) = serde_json::from_str::<Reply>(&line) else {
                    log(format!("plate reader: unreadable reply: {}", line.chars().take(200).collect::<String>()));
                    continue;
                };
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(if reply.ready == Some(true) { Ok(()) } else { Err(reply.error.unwrap_or_else(|| "plate reader failed to start".into())) });
                    continue;
                }
                let Some(tx) = reply.id.and_then(|id| waiting.lock().unwrap().remove(&id)) else { continue };
                let _ = tx.send(match reply.error {
                    Some(error) => Err(error),
                    None => Ok(reply.plates),
                });
            }
            // The helper is gone: everyone still waiting hears so.
            let mut health = health.lock().unwrap();
            health.exited = true;
            health.last_error = "plate reader exited".into();
            health.last_failure = Some(Instant::now());
            for (_, tx) in waiting.lock().unwrap().drain() {
                let _ = tx.send(Err("plate reader stopped".into()));
            }
        });

        match timeout(START_TIMEOUT, ready_rx).await {
            Err(_) => Err(format!("plate reader did not start within {}s", START_TIMEOUT.as_secs())),
            Ok(Err(_)) => Err("plate reader exited while starting".into()),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Ok(Ok(()))) => {
                (self.log)(format!("plate reader ready ({:.1}s)", started.elapsed().as_secs_f64()));
                Ok(Worker { stdin: tokio::sync::Mutex::new(stdin), pending, next_id: AtomicU64::new(1), _child: child })
            }
        }
    }

    /// The plates in a JPEG, best first. Empty when there are none.
    pub async fn read(&self, jpeg: &[u8]) -> Result<Vec<PlateRead>, String> {
        let worker = self.worker().await?;
        let id = worker.next_id.fetch_add(1, Ordering::SeqCst);
        // The helper reads a file, so the frame goes through one.
        let file = std::env::temp_dir().join(format!("plate_{}_{id}_{}.jpg", std::process::id(), fastrand::u32(..)));
        tokio::fs::write(&file, jpeg).await.map_err(|e| e.to_string())?;
        let (tx, rx) = oneshot::channel();
        worker.pending.lock().unwrap().insert(id, tx);
        let request = format!("{}\n", serde_json::json!({ "id": id, "path": file }));
        let sent = worker.stdin.lock().await.write_all(request.as_bytes()).await;
        let result = match sent {
            Err(err) => Err(format!("plate reader not listening: {err}")),
            Ok(()) => match timeout(REQUEST_TIMEOUT, rx).await {
                Err(_) => Err("plate reader timed out".into()),
                Ok(Err(_)) => Err("plate reader stopped".into()),
                Ok(Ok(result)) => result,
            },
        };
        worker.pending.lock().unwrap().remove(&id);
        let _ = tokio::fs::remove_file(&file).await;
        let mut plates = result?;
        plates.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        Ok(plates)
    }

    /// Loads the models now rather than when the first truck arrives.
    pub async fn warm_up(&self) {
        if let Err(err) = self.worker().await {
            (self.log)(format!("plate reader: {err}"));
        }
    }
}

// Against the real helper, so only on request. From the repository root,
// after node scripts/build-plate-reader.js (and on Linux an ONNX Runtime,
// such as the one onnxruntime-node ships):
//   ORT_DYLIB_PATH=$PWD/node_modules/onnxruntime-node/bin/napi-v6/linux/x64/libonnxruntime.so.1 \
//   PLATE_SAMPLE=photo-with-a-plate.jpg cargo test plate_reader -- --ignored --nocapture
#[cfg(test)]
mod tests {
    use super::*;

    fn reader() -> PlateReader {
        let paths = Paths::under(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources"));
        PlateReader::new(paths, Arc::new(|line| println!("{line}")))
    }

    #[test]
    fn reads_a_reply_line() {
        let line = r#"{"id": 1, "plates": [{"text": "5AU5341", "confidence": 0.99, "charConfidence": [0.99], "detConfidence": 0.84, "box": [307, 187, 387, 210]}], "ms": 82}"#;
        let reply: Reply = serde_json::from_str(line).unwrap();
        assert_eq!(reply.id, Some(1));
        assert_eq!(reply.plates[0].text, "5AU5341");
        assert_eq!(reply.plates[0].bounds, [307, 187, 387, 210]);
    }

    #[tokio::test]
    async fn a_missing_reader_says_so_without_trying() {
        let r = PlateReader::new(Paths { exe: "/nowhere/plate-reader".into(), models: "/nowhere/models".into() }, Arc::new(|_| {}));
        let err = r.read(b"jpeg").await.unwrap_err();
        assert!(err.contains("not installed"), "{err}");
        assert!(!r.status().installed);
    }

    #[tokio::test]
    #[ignore]
    async fn plate_reader_reads_the_sample_and_keeps_running() {
        let r = reader();
        let jpeg = std::fs::read(std::env::var("PLATE_SAMPLE").expect("PLATE_SAMPLE=a photo with a plate")).unwrap();
        let started = Instant::now();
        let first = r.read(&jpeg).await.unwrap();
        println!("first read (with model load): {:?} {first:?}", started.elapsed());
        assert!(!first.is_empty());
        // Two at once on the one helper, matched up by id.
        let started = Instant::now();
        let (a, b) = tokio::join!(r.read(&jpeg), r.read(&jpeg));
        println!("two more: {:?}", started.elapsed());
        assert_eq!(a.unwrap()[0].text, first[0].text);
        assert_eq!(b.unwrap()[0].text, first[0].text);
        assert!(r.status().running);
    }
}
