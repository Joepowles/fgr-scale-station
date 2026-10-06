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
    overlayCorner: 'bottom-left',
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
    overlayCorner: 'bottom-right',
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
  }
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
  try {
    cache = merge(DEFAULTS, JSON.parse(fs.readFileSync(file(), 'utf8')));
  } catch {
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
