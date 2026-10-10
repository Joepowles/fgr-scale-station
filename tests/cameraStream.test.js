// Splitting ffmpeg's fragmented MP4 into boxes for the live view's viewers.
// Run with: node --test tests/cameraStream.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { consumeBoxes } = require('../src/main/services/cameraStream');

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
