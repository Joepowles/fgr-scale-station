// Only what can be checked without a LAN: the subnet list shape and that the
// data ports are the NPort's two serial modes.
const test = require('node:test');
const assert = require('node:assert/strict');
const { localSubnets, DATA_PORTS } = require('../src/main/services/nportDiscovery');

test('data ports are Real COM and TCP Server', () => {
  assert.deepEqual(DATA_PORTS, [950, 4001]);
});

test('local subnets are /24 bases with a broadcast address', () => {
  for (const s of localSubnets()) {
    assert.match(s.base, /^\d+\.\d+\.\d+$/);
    assert.equal(s.broadcast, `${s.base}.255`);
    assert.equal(s.self.startsWith(`${s.base}.`), true);
  }
});
