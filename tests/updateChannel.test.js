// Stable follows releases, beta follows pre-releases too, and a beta PC can
// go back to stable. Run with: node --test tests/updateChannel.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { updaterOptionsFor, normalize } = require('../src/main/updateChannel');

test('stable: no pre-releases, no channel file override', () => {
  assert.deepEqual(updaterOptionsFor('stable', '0.1.13'), { allowPrerelease: false, channel: 'latest', allowDowngrade: false });
});

test('beta: pre-releases allowed, beta.yml wanted', () => {
  assert.deepEqual(updaterOptionsFor('beta', '0.1.13'), { allowPrerelease: true, channel: 'beta', allowDowngrade: false });
});

test('a beta install switched to stable may step down to the stable release', () => {
  assert.equal(updaterOptionsFor('stable', '0.2.0-beta.1').allowDowngrade, true);
});

test('anything unknown is stable', () => {
  assert.equal(normalize('nightly'), 'stable');
  assert.equal(normalize(undefined), 'stable');
  assert.equal(normalize('beta'), 'beta');
});
