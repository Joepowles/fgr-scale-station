// The station's settings: one JSON file under the user's app data folder,
// read on start, written on every save. Defaults mirror the scale camera card
// on the Falcon site so a fresh install behaves like the yard's.
const fs = require('fs');
const path = require('path');
const paths = require('./paths');

const DEFAULTS = {
  camera: {
    name: 'Scale',
    host: '',                 // IP or hostname of the camera
    username: 'admin',
    password: '',
    streamUrl: '',            // RTSP for the window (the substream)
    hdStreamUrl: '',          // RTSP for plate reads and photos (the main stream)
    snapshotUrl: '',
    onvifPort: 80,
    showClock: true
  },
  plates: {
    enabled: true,
    minConfidence: 0.6,
    overlayCorner: 'top-left',
    retryAttempts: 10,
    retrySeconds: 3,
    stampPhoto: true
  },
  scale: {
    enabled: false,
    host: '',
    port: 950,
    unit: 'lb',
    minWeight: 4000,
    clearWeight: 1000,
    stableSeconds: 2,
    stableTolerance: 40,
    display: 'picture',       // picture | header | section | none
    overlayCorner: 'bottom-left',
    stampPhoto: true,
    capturePhoto: true,
    readPlate: true,
    soundOnWeighing: true
  },
  history: {
    retentionDays: 7,
    folder: '',               // empty = under app data
    savePhotos: true,
    saveLog: true,
    photoQuality: 85
  },
  window: {
    fullscreen: false,
    alwaysOnTop: false,
    startWithWindows: false
  },
  // Bumped when a default changes in a way existing settings files should
  // pick up; see migrate().
  meta: { version: 2 }
};

// Settings saved by an older version carry the old defaults as if chosen.
// Where they still match those, they move to the new ones; a corner the user
// picked on purpose is left alone.
const migrate = (loaded) => {
  const out = loaded;
  const version = Number(out.meta?.version) || 1;
  if (version < 2) {
    if (out.plates.overlayCorner === 'bottom-left') out.plates.overlayCorner = 'top-left';
    if (out.scale.overlayCorner === 'bottom-right') out.scale.overlayCorner = 'bottom-left';
  }
  out.meta = { ...(out.meta || {}), version: DEFAULTS.meta.version };
  return out;
};

const file = () => path.join(paths.ensureDir(paths.userDataDir()), 'settings.json');

const merge = (base, over) => {
  const out = { ...base };
  for (const [k, v] of Object.entries(over || {})) {
    out[k] = v && typeof v === 'object' && !Array.isArray(v) && base[k] && typeof base[k] === 'object'
      ? merge(base[k], v) : v;
  }
  return out;
};

let cache = null;

const load = () => {
  if (cache) return cache;
  let raw = null;
  try { raw = JSON.parse(fs.readFileSync(file(), 'utf8')); } catch { raw = null; }
  if (raw) {
    // A file from before meta existed is version 1, whatever merge() would
    // fill in; migrate sees the real saved values.
    const savedVersion = Number(raw.meta?.version) || 1;
    cache = migrate({ ...merge(DEFAULTS, raw), meta: { version: savedVersion } });
    if (savedVersion < DEFAULTS.meta.version) {
      try { fs.writeFileSync(file(), JSON.stringify(cache, null, 2)); } catch {}
    }
  } else {
    cache = merge(DEFAULTS, {});
  }
  return cache;
};

const save = (patch) => {
  cache = merge(load(), patch);
  const tmp = `${file()}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(cache, null, 2));
  fs.renameSync(tmp, file());
  return cache;
};

// The history folder in use: the chosen one, or the default under app data.
const historyFolder = (settings = load()) => {
  const chosen = String(settings.history?.folder || '').trim();
  return paths.ensureDir(chosen || path.join(paths.userDataDir(), 'history'));
};

module.exports = { DEFAULTS, load, save, historyFolder, file };
