// The truck scale's serial output, as captured from the yard's NPort on
// 2026-10-06, and the decision of when a weight on it is a vehicle worth
// photographing. Run with: node --test backend/tests/scaleFrames.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { parseFrame, FrameSplitter, WeighingTrigger } = require('../src/main/services/scaleFrames');

const STX = '\x02';
const frame = (weight, status = ' ', unit = 'L', mode = 'G') =>
  `${STX}${String(weight).padStart(7, ' ')}${unit}${mode}${status}\r\n`;

test('parses the yard indicator frame', () => {
  const r = parseFrame(frame(140));
  assert.deepEqual({ weight: r.weight, unit: r.unit, gross: r.gross, motion: r.motion, valid: r.valid },
    { weight: 140, unit: 'lb', gross: true, motion: false, valid: true });
});

test('reads motion, net and kilogram variants', () => {
  assert.equal(parseFrame(frame(48320, 'M')).motion, true);
  assert.equal(parseFrame(frame(48320, ' ', 'L', 'N')).gross, false);
  assert.equal(parseFrame(frame(21900, ' ', 'K')).unit, 'kg');
  assert.equal(parseFrame('  12345 lb GR\r\n').weight, 12345);
  assert.equal(parseFrame('-20LG \r\n').weight, -20);
  assert.equal(parseFrame(frame(0, 'O')).valid, false);
  assert.equal(parseFrame('garbage'), null);
  assert.equal(parseFrame(''), null);
});

test('splits frames however the bytes arrive', () => {
  const s = new FrameSplitter();
  const whole = frame(140) + frame(145);
  const a = s.feed(Buffer.from(whole.slice(0, 5), 'latin1'));
  const b = s.feed(Buffer.from(whole.slice(5), 'latin1'));
  assert.deepEqual([...a, ...b].map((r) => r.weight), [140, 145]);
});

test('a connection opened mid-frame ignores the partial first frame', () => {
  const s = new FrameSplitter({ resync: true });
  const readings = s.feed('0LG \r\n' + frame(140) + frame(141));
  assert.deepEqual(readings.map((r) => r.weight), [140, 141]);
});

test('splits on STX when the indicator sends no line ending', () => {
  const s = new FrameSplitter();
  const readings = s.feed(`${STX}    140LG ${STX}    150LG ${STX}`);
  assert.deepEqual(readings.map((r) => r.weight), [140, 150]);
});

test('fires once per truck, after the weight settles, and re-arms when the deck clears', () => {
  const t = new WeighingTrigger({ minWeight: 5000, clearWeight: 1000, stableSeconds: 2, stableTolerance: 40 });
  const events = [];
  let now = 1000;
  const feed = (weight, motion = false, ms = 50) => {
    const e = t.push({ weight, unit: 'lb', gross: true, motion, valid: true }, now);
    now += ms;
    if (e) events.push(e.type);
  };
  for (let i = 0; i < 40; i++) feed(140);            // empty deck, 2 s
  for (let i = 0; i < 20; i++) feed(20000 + i * 900, true); // rolling on, moving
  for (let i = 0; i < 30; i++) feed(48300 + (i % 3) * 10); // 1.5 s settled: not yet
  assert.deepEqual(events, []);
  for (let i = 0; i < 20; i++) feed(48300 + (i % 3) * 10); // past 2 s
  assert.deepEqual(events, ['weighed']);
  assert.equal(t.lastWeighing.weight >= 48300, true);
  for (let i = 0; i < 60; i++) feed(48310);            // still there: nothing more
  for (let i = 0; i < 10; i++) feed(30000, true);      // rolling off
  for (let i = 0; i < 60; i++) feed(6000);             // a second truck already on? no: deck never cleared
  assert.deepEqual(events, ['weighed']);
  for (let i = 0; i < 5; i++) feed(140);
  assert.deepEqual(events, ['weighed', 'cleared']);
  for (let i = 0; i < 50; i++) feed(31000);
  assert.deepEqual(events, ['weighed', 'cleared', 'weighed']);
});

test('a reading that wobbles beyond the tolerance never settles', () => {
  const t = new WeighingTrigger({ minWeight: 5000, clearWeight: 1000, stableSeconds: 1, stableTolerance: 40 });
  let now = 0; let fired = false;
  for (let i = 0; i < 100; i++) {
    const e = t.push({ weight: 30000 + (i % 2) * 100, unit: 'lb', gross: true, motion: false, valid: true }, now);
    now += 50;
    if (e) fired = true;
  }
  assert.equal(fired, false);
  assert.equal(t.settled, false);
});

test('a truck that settles heavier after the first settle is reweighed, more than once, never lighter', () => {
  const t = new WeighingTrigger({ minWeight: 4000, clearWeight: 1000, stableSeconds: 2, stableTolerance: 40 });
  const events = [];
  let now = 1000;
  const feed = (weight, motion = false, ms = 50) => {
    const e = t.push({ weight, unit: 'lb', gross: true, motion, valid: true }, now);
    now += ms;
    if (e) events.push(e);
  };
  for (let i = 0; i < 40; i++) feed(140);
  for (let i = 0; i < 10; i++) feed(1000 + i * 400, true);   // creeping on
  for (let i = 0; i < 50; i++) feed(4740 + (i % 2) * 10);    // front axle only, steady 2.5 s
  assert.deepEqual(events.map((e) => e.type), ['weighed']);
  assert.equal(events[0].weight >= 4740, true);
  for (let i = 0; i < 20; i++) feed(4760 + i * 20);           // a slow creep within tolerance steps: window never settles at a new value until it holds
  for (let i = 0; i < 20; i++) feed(10000 + i * 1500, true);  // rolling the rest of the way on
  for (let i = 0; i < 50; i++) feed(38200 + (i % 3) * 10);    // the whole tractor, steady
  assert.deepEqual(events.map((e) => e.type), ['weighed', 'reweighed']);
  assert.equal(events[1].previous, events[0].weight);
  assert.equal(events[1].weight >= 38200, true);
  assert.equal(t.lastWeighing.weight, events[1].weight);
  for (let i = 0; i < 60; i++) feed(38210);                   // still there: nothing more
  assert.equal(events.length, 2);
  for (let i = 0; i < 20; i++) feed(40000 + i * 1000, true);  // the trailer comes on
  for (let i = 0; i < 50; i++) feed(79000 + (i % 2) * 20);
  assert.deepEqual(events.map((e) => e.type), ['weighed', 'reweighed', 'reweighed']);
  assert.equal(events[2].previous, events[1].weight);
  for (let i = 0; i < 20; i++) feed(60000, true);             // part of it rolls off: a lighter settle is not a correction
  for (let i = 0; i < 50; i++) feed(50000);
  assert.equal(events.length, 3);
  for (let i = 0; i < 5; i++) feed(140);
  assert.deepEqual(events.map((e) => e.type), ['weighed', 'reweighed', 'reweighed', 'cleared']);
  assert.equal(t.lastWeighing.weight, events[2].weight);
});

test('a wobble that stays inside the tolerance is not a reweigh', () => {
  const t = new WeighingTrigger({ minWeight: 4000, clearWeight: 1000, stableSeconds: 1, stableTolerance: 40 });
  const events = [];
  let now = 0;
  for (let i = 0; i < 200; i++) {
    const e = t.push({ weight: 30000 + (i % 5) * 10, unit: 'lb', gross: true, motion: false, valid: true }, now);
    now += 50;
    if (e) events.push(e.type);
  }
  assert.deepEqual(events, ['weighed']);
});
