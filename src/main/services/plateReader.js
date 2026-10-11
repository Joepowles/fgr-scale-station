const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

// Licence plate reading, done by a helper kept running in the background:
// plate-reader.exe, built from plate-reader/ (Rust). Keeping it running is
// what makes a read cost a tenth of a second rather than the second the
// models take to load.
//
// The models are the open ones from the fast-alpr project: a small YOLO that
// finds plates and a character reader for the crop. The helper runs them on
// ONNX Runtime exactly as fast-alpr does, and was checked against it read for
// read (plate-reader/compare/); it replaced a bundled Python with fast-alpr,
// which was most of the installer.
//
// The helper is optional: a build without it simply reports plates as
// unavailable, and nothing else about detection changes.
const paths = require('../paths');
const REQUEST_TIMEOUT_MS = 20000;
const START_TIMEOUT_MS = 30000;
const RESTART_BACKOFF_MS = 30000;
// Noise from onnxruntime inside an LXC container: it tries to pin its threads
// to cores and the container forbids it. Harmless, and it says so eight times
// per model.
const IGNORED_STDERR = ['pthread_setaffinity_np'];

// PLATE_READER_EXE points at another build (development).
const readerPath = () => process.env.PLATE_READER_EXE || paths.plateReaderExe();

const isInstalled = () => {
  try { fs.accessSync(readerPath(), fs.constants.X_OK); return true; } catch { return false; }
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
    lastError = `plate reader not installed (nothing at ${readerPath()}; the app was built without scripts/build-plate-reader.js)`;
    return Promise.reject(new Error(lastError));
  }
  if (Date.now() - lastFailureAt < RESTART_BACKOFF_MS) {
    return Promise.reject(new Error(lastError || 'plate reader recently failed; waiting before retrying'));
  }

  const proc = spawn(readerPath(), ['--models', paths.plateModelsDir()], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  const w = { proc, ready: false, pending: new Map(), nextId: 1, startedAt: Date.now() };
  worker = w;

  let stdoutTail = '';
  let stderrTail = '';
  let resolveReady;
  let rejectReady;
  readyPromise = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const readyTimer = setTimeout(() => {
    if (!w.ready) {
      lastError = `plate reader did not start within ${START_TIMEOUT_MS / 1000}s`;
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
  reader: readerPath(),
  running: !!(worker && worker.ready),
  lastError
});

const stop = () => killWorker();

module.exports = { readPlates, warmUp, status, stop, isInstalled };
