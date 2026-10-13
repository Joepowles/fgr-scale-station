// Which detection the weighing photo reads the plate from. Run with:
// node --test tests/vehicleDetector.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { VEHICLE_CLASSES } = require('../src/main/services/vehicleDetector');
const { largestVehicle } = require('../src/main/services/weighingPipeline');

const detection = (label, score, box) => ({ label, score, box, isVehicle: VEHICLE_CLASSES.has(label) });

test('a truck on the scale taken for a train is still the one weighed, not a truck parked behind it', () => {
  // The yard at night, 2026-10-13: the boxes as the model gave them.
  const onScale = detection('train', 0.35, [0.63, 0.16, 1.0, 0.74]);
  const parked = detection('truck', 0.53, [0.0, 0.37, 0.24, 0.55]);
  assert.equal(largestVehicle([parked, onScale]), onScale);
});

test('things that are not vehicles are never weighed', () => {
  assert.equal(largestVehicle([detection('person', 0.9, [0, 0, 1, 1])]), null);
});
