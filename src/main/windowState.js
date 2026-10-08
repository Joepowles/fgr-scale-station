// Where the window was last: its size and position, and whether it was
// maximised, kept in window-state.json beside the settings so the app comes
// back where it was left. A position on a monitor that is no longer there
// (a second screen unplugged, a different resolution) is dropped and the
// window opens at its default size on the main screen.
const fs = require('fs');
const path = require('path');

const FILE = 'window-state.json';
const SAVE_DELAY_MS = 500;
const MIN_VISIBLE = 100; // px of the window that must sit on some screen

const stateFile = (dir) => path.join(dir, FILE);

const read = (dir) => {
  try {
    const s = JSON.parse(fs.readFileSync(stateFile(dir), 'utf8'));
    return s && typeof s === 'object' ? s : null;
  } catch { return null; }
};

const write = (dir, state) => {
  try { fs.writeFileSync(stateFile(dir), JSON.stringify(state, null, 2)); } catch {}
};

// The bounds to open with, or null when the saved ones would land off every
// screen. displays: [{ workArea: { x, y, width, height } }].
const placementFor = (saved, displays) => {
  if (!saved || ![saved.x, saved.y, saved.width, saved.height].every(Number.isFinite)) return null;
  if (saved.width < 200 || saved.height < 150) return null;
  const visible = (displays || []).some(({ workArea: a }) => {
    const w = Math.min(saved.x + saved.width, a.x + a.width) - Math.max(saved.x, a.x);
    const h = Math.min(saved.y + saved.height, a.y + a.height) - Math.max(saved.y, a.y);
    return w >= MIN_VISIBLE && h >= MIN_VISIBLE;
  });
  return visible ? { x: saved.x, y: saved.y, width: saved.width, height: saved.height } : null;
};

// Save the window's placement as it changes, and once more when it closes.
// Fullscreen and maximised sizes are the screen's, not the user's, so the
// last normal bounds are what is kept, with the maximised flag beside them.
const track = (win, dir) => {
  let state = read(dir) || {};
  let timer = null;
  const capture = () => {
    if (win.isDestroyed()) return;
    if (!win.isFullScreen() && !win.isMaximized() && !win.isMinimized()) state = { ...state, ...win.getNormalBounds() };
    if (!win.isFullScreen()) state.maximized = win.isMaximized();
  };
  const save = () => { capture(); write(dir, state); };
  const later = () => { clearTimeout(timer); timer = setTimeout(save, SAVE_DELAY_MS); };
  win.on('resize', later);
  win.on('move', later);
  win.on('maximize', later);
  win.on('unmaximize', later);
  win.on('close', () => { clearTimeout(timer); save(); });
  return { save };
};

module.exports = { read, write, placementFor, track, FILE };
