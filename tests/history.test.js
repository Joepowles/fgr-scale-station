// The local history: entries written, updated, listed and pruned with their
// photos. Run with: node --test tests/history.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { History } = require('../src/main/services/history');

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), 'station-history-'));

test('records entries with photos and lists newest first', () => {
  const h = new History({ folder: tmp(), retentionDays: 7 });
  const a = h.record({ type: 'weighing', weight: 12200, unit: 'lb', at: '2026-10-06T14:20:10Z' }, Buffer.from('jpeg-a'));
  const b = h.record({ type: 'weighing', weight: 48320, unit: 'lb', plate: { text: 'ABC123', confidence: 0.93 }, at: '2026-10-06T15:00:00Z' }, Buffer.from('jpeg-b'));
  assert.equal(fs.existsSync(h.photoPath(a.photo)), true);
  const list = h.list();
  assert.deepEqual(list.map((e) => e.weight), [48320, 12200]);
  assert.equal(list[0].plate.text, 'ABC123');
  assert.equal(b.photo.endsWith('.jpg'), true);
});

test('updates an entry in place', () => {
  const h = new History({ folder: tmp(), retentionDays: 7 });
  const e = h.record({ weight: 1000, unit: 'lb' });
  h.update(e.id, { plate: { text: 'NEW', confidence: 0.95 } });
  assert.equal(h.list()[0].plate.text, 'NEW');
  assert.equal(h.read().length, 1);
});

test('prunes entries and photos past the retention period', () => {
  const h = new History({ folder: tmp(), retentionDays: 7 });
  const old = h.record({ weight: 1, unit: 'lb', at: new Date(Date.now() - 10 * 86400000).toISOString() }, Buffer.from('old'));
  const fresh = h.record({ weight: 2, unit: 'lb' }, Buffer.from('new'));
  fs.writeFileSync(h.photoPath('orphan.jpg'), 'x');
  const result = h.prune();
  assert.equal(result.dropped, 1);
  assert.equal(fs.existsSync(h.photoPath(old.photo)), false);
  assert.equal(fs.existsSync(h.photoPath(fresh.photo)), true);
  assert.equal(fs.existsSync(h.photoPath('orphan.jpg')), false);
  assert.deepEqual(h.list().map((e) => e.weight), [2]);
});

test('can keep the log without photos', () => {
  const h = new History({ folder: tmp(), retentionDays: 7, savePhotos: false });
  const e = h.record({ weight: 5, unit: 'lb' }, Buffer.from('x'));
  assert.equal(e.photo, null);
  assert.equal(fs.readdirSync(path.join(h.folder, 'photos')).length, 0);
});

test('an update can replace the photo under the same name', () => {
  const h = new History({ folder: tmp(), retentionDays: 7 });
  const e = h.record({ weight: 4740, unit: 'lb' }, Buffer.from('front-axle'));
  const updated = h.update(e.id, { weight: 38200, corrected: { from: 4740 } }, Buffer.from('whole-truck'));
  assert.equal(updated.photo, e.photo);
  assert.equal(fs.readFileSync(h.photoPath(e.photo), 'utf8'), 'whole-truck');
  assert.equal(h.list()[0].weight, 38200);
  assert.equal(h.read().length, 1);
});
