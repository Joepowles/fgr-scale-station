// Splitting ffmpeg's fragmented MP4 into boxes for the live view's viewers.
// Run with: node --test tests/cameraStream.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { consumeBoxes, trackLag } = require('../src/main/services/cameraStream');

const box = (type, payload = 'x') => {
  const body = Buffer.from(payload);
  const head = Buffer.alloc(8);
  head.writeUInt32BE(8 + body.length, 0);
  head.write(type, 4, 'ascii');
  return Buffer.concat([head, body]);
};
const typesIn = (buf) => {
  const types = [];
  for (let at = 0; at < buf.length; at += buf.readUInt32BE(at)) types.push(buf.toString('ascii', at + 4, at + 8));
  return types;
};
const viewer = () => {
  const writes = [];
  return { writes, writableEnded: false, destroyed: false, write: (b) => writes.push(Buffer.from(b)), end() {}, types: () => writes.map(typesIn) };
};
const fresh = () => ({
  subscribers: new Set(), pending: new Set(), buffer: Buffer.alloc(0),
  initParts: [], initSegment: null, initReady: false, heldMoof: null, lingerTimer: null
});

test('keeps ftyp + moov as the init segment and hands it to waiting viewers', () => {
  const state = fresh();
  const early = viewer();
  state.pending.add(early);
  consumeBoxes(state, Buffer.concat([box('ftyp'), box('moov')]));
  assert.deepEqual(typesIn(state.initSegment), ['ftyp', 'moov']);
  assert.deepEqual(early.types(), [['ftyp', 'moov']]);
});

test('sends each moof together with its mdat, however the bytes are split', () => {
  const state = fresh();
  const v = viewer();
  state.pending.add(v);
  const stream = Buffer.concat([box('ftyp'), box('moov'), box('moof', 'aa'), box('mdat', 'frame1'), box('moof', 'bb'), box('mdat', 'frame2')]);
  for (let i = 0; i < stream.length; i += 5) consumeBoxes(state, stream.subarray(i, i + 5));
  assert.deepEqual(v.types(), [['ftyp', 'moov'], ['moof', 'mdat'], ['moof', 'mdat']]);
});

test('a viewer who joins between a moof and its mdat starts at the next whole fragment', () => {
  const state = fresh();
  consumeBoxes(state, Buffer.concat([box('ftyp'), box('moov'), box('moof')]));
  // Joins now, as streamRtspToFmp4 does: the init segment, then live boxes.
  const late = viewer();
  late.write(state.initSegment);
  state.subscribers.add(late);
  consumeBoxes(state, Buffer.concat([box('mdat'), box('moof'), box('mdat')]));
  // It used to get that first mdat on its own, with no moof to decode it by.
  assert.deepEqual(late.types(), [['ftyp', 'moov'], ['moof', 'mdat'], ['moof', 'mdat']]);
});

test('measures how far ffmpeg falls behind the clock from its time= stats', () => {
  const state = {};
  const line = (t) => `frame=  10 fps= 15 q=-1.0 size=  1kB time=${t} bitrate= 1kbits/s speed=1x\r`;
  // The first reading is the baseline: connecting is not lag.
  assert.equal(trackLag(state, line('00:00:01.00'), 100000), 0);
  assert.equal(trackLag(state, line('00:00:11.00'), 110000), 0);
  // Ten more seconds of clock, six of media: four seconds behind.
  assert.equal(trackLag(state, line('00:00:17.00'), 120000), 4);
  assert.equal(trackLag(state, line('00:01:02.50'), 166000), 4.5);
  assert.equal(trackLag(state, 'time=N/A', 170000), 0);
});
