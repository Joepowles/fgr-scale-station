// The window comes back where it was, unless that is on a screen that is
// gone. Run with: node --test tests/windowState.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { placementFor, read, write } = require('../src/main/windowState');

const main = { workArea: { x: 0, y: 0, width: 1920, height: 1040 } };
const second = { workArea: { x: 1920, y: 0, width: 1920, height: 1080 } };

test('restores bounds that sit on a connected screen', () => {
  assert.deepEqual(placementFor({ x: 100, y: 50, width: 1280, height: 800, maximized: false }, [main]), { x: 100, y: 50, width: 1280, height: 800 });
  assert.deepEqual(placementFor({ x: 2000, y: 100, width: 1280, height: 800 }, [main, second]), { x: 2000, y: 100, width: 1280, height: 800 });
});

test('drops bounds on a screen that is no longer there, or nonsense', () => {
  assert.equal(placementFor({ x: 2000, y: 100, width: 1280, height: 800 }, [main]), null);
  assert.equal(placementFor({ x: -1300, y: 100, width: 1280, height: 800 }, [main]), null);
  assert.equal(placementFor({ x: 100, y: 100, width: 50, height: 800 }, [main]), null);
  assert.equal(placementFor({ x: 'a', y: 100, width: 1280, height: 800 }, [main]), null);
  assert.equal(placementFor(null, [main]), null);
  assert.equal(placementFor({ x: 1850, y: 1000, width: 1280, height: 800 }, [main]), null); // only a sliver showing
});

test('reads back what was written, and nothing when there is no file', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'station-window-'));
  assert.equal(read(dir), null);
  write(dir, { x: 1, y: 2, width: 800, height: 600, maximized: true });
  assert.deepEqual(read(dir), { x: 1, y: 2, width: 800, height: 600, maximized: true });
});
