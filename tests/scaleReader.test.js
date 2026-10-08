// The reader made afresh on a settings save carries over the truck already
// on the deck. Run with: node --test tests/scaleReader.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { ScaleReader } = require('../src/main/services/scaleReader');

const settings = { host: '127.0.0.1', port: 1, unit: 'lb', minWeight: 4000, clearWeight: 1000, stableSeconds: 2, stableTolerance: 40 };
const frame = (weight) => Buffer.from(`\x02${String(weight).padStart(7, ' ')}LG \r\n`, 'latin1');

test('a reader that adopts a loaded one does not weigh the same truck again', () => {
  const old = new ScaleReader(settings);
  const weighed = [];
  old.on('weighed', (w) => weighed.push(w.weight));
  const realNow = Date.now; let clock = 0;
  Date.now = () => clock;
  let fresh = null;
  try {
    for (let i = 0; i < 50; i++) { clock += 50; old.onData(frame(32000)); }
    assert.deepEqual(weighed, [32000]);
    assert.equal(old.loaded, true);
    fresh = new ScaleReader(settings).adopt(old);
    fresh.on('weighed', (w) => weighed.push(w.weight));
    assert.equal(fresh.loaded, true);
    assert.equal(fresh.status.lastWeighing.weight, 32000);
    for (let i = 0; i < 100; i++) { clock += 50; fresh.onData(frame(32000)); }
    assert.deepEqual(weighed, [32000]);
    for (let i = 0; i < 50; i++) { clock += 50; fresh.onData(frame(0)); }
    for (let i = 0; i < 50; i++) { clock += 50; fresh.onData(frame(41000)); }
    assert.deepEqual(weighed, [32000, 41000]);
  } finally {
    Date.now = realNow;
    old.stop(); fresh?.stop();
  }
});
