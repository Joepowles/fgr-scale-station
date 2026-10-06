const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

// Licence plate reading, done by a Python helper kept running in the
// background (backend/scripts/plate_reader_worker.py).
//
// The models are the open ones from the fast-alpr project: a small YOLO that
// finds plates and a character reader for the crop. They run on the CPU in a
// few hundred milliseconds, which is plenty for a weighbridge. They are in
// Python rather than wired into onnxruntime-node like the vehicle model because
// the project ships preprocessing and decoding that would otherwise have to be
// reproduced exactly; this keeps that in one place that is tested upstream.
//
// The helper is optional: a box without it (the dev web tier, DigitalOcean)
// simply reports plates as unavailable, and nothing else about detection
// changes. Install it with backend/scripts/setup-plate-reader.sh on the box
// that runs vehicle detection.
const paths = require('../paths');
const WORKER_SCRIPT = paths.plateWorkerScript();
const DEFAULT_PYTHON = paths.pythonExe();
const REQUEST_TIMEOUT_MS = 20000;
const START_TIMEOUT_MS = 90000;      // first run downloads the models
const RESTART_BACKOFF_MS = 30000;
// Noise from onnxruntime inside an LXC container: it tries to pin its threads
// to cores and the container forbids it. Harmless, and it says so eight times
// per model.
const IGNORED_STDERR = ['pthread_setaffinity_np', 'Downloading '];

const pythonPath = () => process.env.PLATE_READER_PYTHON || DEFAULT_PYTHON;

const isInstalled = () => {
  try { fs.accessSync(pythonPath(), fs.constants.X_OK); return true; } catch { return false; }
};

let worker = null;          // { proc, ready, pending: Map, nextId, startedAt }
let readyPromise = null;
let lastFailureAt = 0;
let lastError = '';

const killWorker = () => {
  if (!worker) return;
  const w = worker;
  worker = null;
  readyPromise = null;
  for (const { reject, timer } of w.pending.values()) {
    clearTimeout(timer);
    reject(new Error('plate reader stopped'));
  }
  w.pending.clear();
  try { w.proc.kill('SIGKILL'); } catch { /* already gone */ }
};

const startWorker = () => {
  if (readyPromise) return readyPromise;
  if (!isInstalled()) {
    lastError = `plate reader not installed (no Python at ${pythonPath()}; the app was built without scripts/build-python.js)`;
    return Promise.reject(new Error(lastError));
  }
  if (Date.now() - lastFailureAt < RESTART_BACKOFF_MS) {
    return Promise.reject(new Error(lastError || 'plate reader recently failed; waiting before retrying'));
  }

  // The embedded Python's home holds the pre-downloaded models (see
  // scripts/build-python.js); fast-alpr looks for them under ~/.cache.
  const env = { ...process.env, PYTHONIOENCODING: 'utf-8', PYTHONUTF8: '1' };
  if (fs.existsSync(paths.pythonHome())) {
    env.USERPROFILE = paths.pythonHome();
    env.HOME = paths.pythonHome();
    env.HF_HUB_OFFLINE = '1';
  }
  const proc = spawn(pythonPath(), [WORKER_SCRIPT], { stdio: ['pipe', 'pipe', 'pipe'], env, windowsHide: true });
  const w = { proc, ready: false, pending: new Map(), nextId: 1, startedAt: Date.now() };
  worker = w;

  let stdoutTail = '';
  let stderrTail = '';
  let resolveReady;
  let rejectReady;
  readyPromise = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const readyTimer = setTimeout(() => {
    if (!w.ready) {
      lastError = 'plate reader did not start within 90s';
      lastFailureAt = Date.now();
      rejectReady(new Error(lastError));
      killWorker();
    }
  }, START_TIMEOUT_MS);

  proc.stdout.on('data', (chunk) => {
    const lines = (stdoutTail + chunk.toString()).split('\n');
    stdoutTail = lines.pop();
    for (const line of lines) {
      if (!line.trim()) continue;
      let message;
      try { message = JSON.parse(line); } catch { console.warn('[PlateReader] unreadable reply:', line.slice(0, 200)); continue; }
      if (!w.ready) {
        clearTimeout(readyTimer);
        if (message.ready) {
          w.ready = true;
          lastError = '';
          console.log(`[PlateReader] worker ready (${Math.round((Date.now() - w.startedAt) / 1000)}s)`);
          resolveReady();
        } else {
          lastError = message.error || 'plate reader failed to start';
          lastFailureAt = Date.now();
          rejectReady(new Error(lastError));
          killWorker();
        }
        continue;
      }
      const waiting = w.pending.get(message.id);
      if (!waiting) continue;
      w.pending.delete(message.id);
      clearTimeout(waiting.timer);
      if (message.error) waiting.reject(new Error(message.error));
      else waiting.resolve({ plates: message.plates || [], ms: message.ms || 0 });
    }
  });

  proc.stderr.on('data', (chunk) => {
    const lines = (stderrTail + chunk.toString()).split(/\r?\n/);
    stderrTail = lines.pop();
    for (const line of lines) {
      const msg = line.trim();
      if (!msg || IGNORED_STDERR.some((s) => msg.includes(s))) continue;
      console.warn('[PlateReader]', msg.slice(0, 300));
    }
  });

  proc.on('error', (err) => {
    lastError = `plate reader could not be started: ${err.message}`;
    lastFailureAt = Date.now();
    if (!w.ready) rejectReady(new Error(lastError));
    killWorker();
  });

  proc.on('close', (code) => {
    if (worker !== w) return;
    lastError = `plate reader exited (code ${code})`;
    lastFailureAt = Date.now();
    console.warn(`[PlateReader] ${lastError}`);
    if (!w.ready) rejectReady(new Error(lastError));
    killWorker();
  });

  return readyPromise;
};

/**
 * Read the plates in a JPEG. Resolves to { plates: [...], ms } where each
 * plate is { text, confidence, charConfidence, detConfidence, box } with the
 * box in pixels of the image given. An empty list means no plate was found.
 */
const readPlates = async (jpegBuffer) => {
  await startWorker();
  const w = worker;
  if (!w || !w.ready) throw new Error(lastError || 'plate reader not running');
  const tmpFile = path.join(os.tmpdir(), `plate_${Date.now()}_${Math.random().toString(36).slice(2)}.jpg`);
  fs.writeFileSync(tmpFile, jpegBuffer);
  const id = w.nextId++;
  try {
    return await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        w.pending.delete(id);
        reject(new Error('plate reader timed out'));
      }, REQUEST_TIMEOUT_MS);
      w.pending.set(id, { resolve, reject, timer });
      w.proc.stdin.write(`${JSON.stringify({ id, path: tmpFile })}\n`);
    });
  } finally {
    try { fs.unlinkSync(tmpFile); } catch { /* already gone */ }
  }
};

// Start loading the models now rather than when the first truck arrives.
const warmUp = () => startWorker().catch((err) => {
  console.warn(`[PlateReader] ${err.message}`);
});

const status = () => ({
  installed: isInstalled(),
  python: pythonPath(),
  running: !!(worker && worker.ready),
  lastError
});

const stop = () => killWorker();

module.exports = { readPlates, warmUp, status, stop, isInstalled };
