// A heavier settle corrects the weighing already written instead of adding a
// second one. Camera, detector and plate reader are stubbed; the history is
// real, in a temp folder. Run with: node --test tests/weighingPipeline.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const stub = (rel, exports) => { const id = require.resolve(rel); require.cache[id] = { id, filename: id, loaded: true, exports }; };
const snapshots = [];
stub('../src/main/services/rtspSnapshot', { captureRtspSnapshot: async (url) => { snapshots.push(url); if (url === 'rtsp://down') throw new Error('camera down'); return Buffer.from(`frame-${snapshots.length}`); } });
stub('../src/main/services/vehicleDetector', {
  detect: async () => ({ width: 640, height: 480, detections: [{ box: [0.1, 0.1, 0.9, 0.9], isVehicle: true, label: 'truck', score: 0.9 }] }),
  annotate: async (frame, { weight, plate }) => Buffer.from(`${frame.toString()}|${weight?.text || ''}|${plate?.text || ''}`)
});
stub('../src/main/services/plateReader', { readPlates: async () => ({ plates: [{ text: 'ABC123', confidence: 0.95 }] }) });
stub('sharp', () => ({ metadata: async () => ({ width: 640, height: 480 }), extract: () => ({ jpeg: () => ({ toBuffer: async () => Buffer.from('crop') }) }) }));

const { WeighingPipeline } = require('../src/main/services/weighingPipeline');
const { History } = require('../src/main/services/history');

const settingsFor = (streamUrl) => () => ({
  camera: { streamUrl, hdStreamUrl: streamUrl, username: '', password: '' },
  scale: { capturePhoto: true, stampPhoto: true, overlayCorner: 'bottom-left', readPlate: true },
  plates: { enabled: true, minConfidence: 0.6, retryAttempts: 1, retrySeconds: 1, stampPhoto: true, overlayCorner: 'top-left' }
});

test('a heavier settle corrects the entry: new weight, new photo, plate kept', async () => {
  const history = new History({ folder: fs.mkdtempSync(path.join(os.tmpdir(), 'station-pipe-')) });
  const pipeline = new WeighingPipeline({ settings: settingsFor('rtsp://cam'), history, isLoaded: () => true, onPlate: () => {}, log: () => {} });
  const first = await pipeline.onWeighed({ weight: 4740, unit: 'lb', at: Date.now() });
  assert.equal(first.weight, 4740);
  assert.equal(first.plate.text, 'ABC123');
  assert.equal(fs.readFileSync(history.photoPath(first.photo), 'utf8'), 'frame-1|4,740 lb|ABC123');

  const corrected = await pipeline.onReweighed({ weight: 38200, previous: 4740, unit: 'lb', at: Date.now() });
  assert.equal(corrected.id, first.id);
  assert.equal(corrected.weight, 38200);
  assert.equal(corrected.corrected.from, 4740);
  assert.equal(corrected.plate.text, 'ABC123');
  assert.equal(history.read().length, 1);
  assert.equal(fs.readFileSync(history.photoPath(first.photo), 'utf8').endsWith('|38,200 lb|ABC123'), true);
  assert.notEqual(fs.readFileSync(history.photoPath(first.photo), 'utf8'), 'frame-1|38,200 lb|ABC123'); // a fresh picture

  const again = await pipeline.onReweighed({ weight: 79000, previous: 38200, unit: 'lb', at: Date.now() });
  assert.equal(again.id, first.id);
  assert.equal(again.weight, 79000);
  assert.equal(history.read().length, 1);

  pipeline.onCleared();
  assert.equal(await pipeline.onReweighed({ weight: 90000, previous: 79000, unit: 'lb', at: Date.now() }), null);
});

test('with the camera down the first picture is re-stamped with the new weight', async () => {
  const history = new History({ folder: fs.mkdtempSync(path.join(os.tmpdir(), 'station-pipe-')) });
  const pipeline = new WeighingPipeline({ settings: settingsFor('rtsp://cam'), history, isLoaded: () => true, onPlate: () => {}, log: () => {} });
  const first = await pipeline.onWeighed({ weight: 4740, unit: 'lb', at: Date.now() });
  const firstPhoto = fs.readFileSync(history.photoPath(first.photo), 'utf8');
  pipeline.settings = settingsFor('rtsp://down');
  const corrected = await pipeline.onReweighed({ weight: 38200, previous: 4740, unit: 'lb', at: Date.now() });
  assert.equal(corrected.weight, 38200);
  assert.equal(fs.readFileSync(history.photoPath(first.photo), 'utf8'), firstPhoto.replace('4,740 lb', '38,200 lb'));
});

test('without a camera the weight alone is corrected', async () => {
  const history = new History({ folder: fs.mkdtempSync(path.join(os.tmpdir(), 'station-pipe-')) });
  const pipeline = new WeighingPipeline({ settings: settingsFor(''), history, isLoaded: () => true, onPlate: () => {}, log: () => {} });
  const first = await pipeline.onWeighed({ weight: 4740, unit: 'lb', at: Date.now() });
  const corrected = await pipeline.onReweighed({ weight: 38200, previous: 4740, unit: 'lb', at: Date.now() });
  assert.equal(corrected.id, first.id);
  assert.equal(corrected.weight, 38200);
  assert.equal(corrected.photo, null);
  assert.equal(history.read().length, 1);
});

test('a reweigh that lands while the first photo is still being taken is applied once the entry is written', async () => {
  const history = new History({ folder: fs.mkdtempSync(path.join(os.tmpdir(), 'station-pipe-')) });
  const pipeline = new WeighingPipeline({ settings: settingsFor('rtsp://cam'), history, isLoaded: () => true, onPlate: () => {}, log: () => {} });
  const weighed = pipeline.onWeighed({ weight: 4740, unit: 'lb', at: Date.now() });
  assert.equal(await pipeline.onReweighed({ weight: 38200, previous: 4740, unit: 'lb', at: Date.now() }), null);
  const first = await weighed;
  assert.equal(first.weight, 4740);
  const [entry] = history.read();
  assert.equal(entry.id, first.id);
  assert.equal(entry.weight, 38200);
  assert.equal(entry.corrected.from, 4740);
  assert.equal(history.read().length, 1);
});
