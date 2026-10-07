// Settings defaults and the one-time move of the overlay corners.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const withTempData = (fn) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'station-settings-'));
  const prev = process.env.LOCALAPPDATA;
  process.env.LOCALAPPDATA = dir;
  delete require.cache[require.resolve('../src/main/settings')];
  delete require.cache[require.resolve('../src/main/paths')];
  try { return fn(path.join(dir, 'fgr-scale-station', 'settings.json'), require('../src/main/settings')); }
  finally { process.env.LOCALAPPDATA = prev; }
};

test('fresh defaults put the plate top left and the weight bottom left', () => withTempData((_file, settings) => {
  const s = settings.load();
  assert.equal(s.plates.overlayCorner, 'top-left');
  assert.equal(s.scale.overlayCorner, 'bottom-left');
  assert.equal(s.history.retentionDays, 7);
}));

test('a version-1 file still on the old corners moves to the new ones', () => withTempData((file, settings) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify({ plates: { overlayCorner: 'bottom-left' }, scale: { overlayCorner: 'bottom-right', minWeight: 4000 } }));
  const s = settings.load();
  assert.equal(s.plates.overlayCorner, 'top-left');
  assert.equal(s.scale.overlayCorner, 'bottom-left');
  assert.equal(s.scale.minWeight, 4000);
  assert.equal(JSON.parse(fs.readFileSync(file, 'utf8')).meta.version, 2);
}));

test('a corner the user chose is left alone', () => withTempData((file, settings) => {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, JSON.stringify({ plates: { overlayCorner: 'top-right' }, scale: { overlayCorner: 'bottom-right' } }));
  const s = settings.load();
  assert.equal(s.plates.overlayCorner, 'top-right');
  assert.equal(s.scale.overlayCorner, 'bottom-left');
}));
