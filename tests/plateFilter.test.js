// Plates are kept, signage is not. Run with: node --test tests/plateFilter.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { looksLikePlate, pickPlate } = require('../src/main/services/plateFilter');

test('real plates pass', () => {
  for (const t of ['P1090037', '55001R', 'ABC 123', 'AB-1234', '1234567', 'A1B2']) assert.equal(looksLikePlate(t), true, t);
});

test('words, stickers and junk do not', () => {
  for (const t of ['RUBLE', 'TRUCK SALES', 'KENWORTH', 'A1', '', 'P10900371234']) assert.equal(looksLikePlate(t), false, t);
});

test('an all-letter plate is allowed when digits are not required', () => {
  assert.equal(looksLikePlate('FALCON', { requireDigit: false }), true);
  assert.equal(looksLikePlate('RUBLE', { requireDigit: false, ignoreWords: 'ruble, kenworth' }), false);
});

test('picks the plate, not the placard, and never a text seen twice', () => {
  const finds = [
    { text: 'RUBLE', confidence: 0.97, box: [10, 10, 60, 30] },
    { text: 'RUBLE', confidence: 0.96, box: [200, 10, 250, 30] },
    { text: 'P1090037', confidence: 0.91, box: [100, 50, 160, 70] }
  ];
  assert.equal(pickPlate(finds).text, 'P1090037');
  assert.equal(pickPlate(finds.slice(0, 2)), null);
  assert.equal(pickPlate([{ text: 'AB1234', confidence: 0.8, box: [0, 0, 1, 1] }, { text: 'AB 1234', confidence: 0.7, box: [50, 0, 51, 1] }]), null);
  assert.equal(pickPlate([]), null);
  assert.equal(pickPlate([{ text: 'AB1234', confidence: 0.8 }, { text: 'XY9876', confidence: 0.9 }]).text, 'XY9876');
});
