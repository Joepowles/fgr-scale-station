//! Licence plate reading, done by a Python helper kept running in the
//! background (resources/python-worker/plate_reader_worker.py).
//!
//! The models are the open ones from the fast-alpr project: a small YOLO
//! that finds plates and a character reader for the crop. They run on the CPU
//! in well under a second. They stay in Python for now because the project
//! ships preprocessing and decoding that would otherwise have to be
//! reproduced exactly; reading them from Rust with `ort` is the next
//! experiment, once there are yard photos to check it against.
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

const START_TIMEOUT: Duration = Duration::from_secs(90); // a first run may download the models
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RESTART_BACKOFF: Duration = Duration::from_secs(30);
// Noise from onnxruntime inside an LXC container: it tries to pin its
// threads to cores and the container forbids it. Harmless, and it says so
// eight times per model.
const IGNORED_STDERR: [&str; 3] = ["pthread_setaffinity_np", "Downloading ", "INFO:"];

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

/// Where the reader's Python and script are.
#[derive(Debug, Clone)]
pub struct Paths {
    pub python: PathBuf,
    pub script: PathBuf,
    /// The embedded Python's home, holding the pre-downloaded models; when
    /// present the worker is pointed at it and kept offline.
    pub home: Option<PathBuf>,
}

impl Paths {
    /// The installed layout under the app's resources: python/ (Windows
    /// builds only, see scripts) and python-worker/. PLATE_READER_PYTHON
    /// overrides the Python, for development against a venv.
    pub fn under(resources: &std::path::Path) -> Self {
        let python_dir = resources.join("python");
        let bundled = if cfg!(windows) { python_dir.join("python.exe") } else { python_dir.join("bin").join("python") };
        let home = python_dir.join("home");
        Self {
            python: std::env::var_os("PLATE_READER_PYTHON").map(PathBuf::from).unwrap_or(bundled),
            script: resources.join("python-worker").join("plate_reader_worker.py"),
            home: home.is_dir().then_some(home),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub installed: bool,
    pub python: String,
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
        self.paths.python.is_file() && self.paths.script.is_file()
    }

    pub fn status(&self) -> Status {
        let health = self.health.lock().unwrap();
        // Locked means starting, which is not running yet.
        let running = self.worker.try_lock().map(|w| w.is_some() && !health.exited).unwrap_or(false);
        Status { installed: self.installed(), python: self.paths.python.display().to_string(), running, last_error: health.last_error.clone() }
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
            return Err(self.fail(format!("plate reader not installed (no Python at {})", self.paths.python.display())));
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
        let mut cmd = Command::new(&self.paths.python);
        cmd.arg(&self.paths.script)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(home) = &self.paths.home {
            // fast-alpr looks for its models under ~/.cache.
            cmd.env("USERPROFILE", home).env("HOME", home).env("HF_HUB_OFFLINE", "1");
        }
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

// Against the real helper in a venv, so only on request:
//   python3 -m venv .venv-plates && .venv-plates/bin/pip install fast-alpr onnxruntime
//   PLATE_SAMPLE=photo-with-a-plate.jpg cargo test plate_reader -- --ignored --nocapture
#[cfg(test)]
mod tests {
    use super::*;

    fn reader() -> PlateReader {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let paths = Paths { python: root.join(".venv-plates/bin/python"), script: root.join("resources/python-worker/plate_reader_worker.py"), home: None };
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
    async fn a_missing_python_says_so_without_trying() {
        let r = PlateReader::new(Paths { python: "/nowhere/python".into(), script: "/nowhere/w.py".into(), home: None }, Arc::new(|_| {}));
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
