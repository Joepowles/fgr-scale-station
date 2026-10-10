// Only what can be checked without a camera: how a ProbeMatch is read and
// which ports the sweep knocks on.
const test = require('node:test');
const assert = require('node:assert/strict');
const http = require('node:http');
const { parseProbeMatch, onvifAt, ONVIF_PORTS } = require('../src/main/services/cameraDiscovery');

const match = (xaddrs, scopes) => '<?xml version="1.0"?><SOAP-ENV:Envelope><SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch>'
  + `<d:Scopes>${scopes}</d:Scopes><d:XAddrs>${xaddrs}</d:XAddrs>`
  + '</d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>';

test('a ProbeMatch gives the device address, name and hardware', () => {
  const cam = parseProbeMatch(match(
    'http://192.168.1.229:8899/onvif/device_service',
    'onvif://www.onvif.org/name/Gate+Cam onvif://www.onvif.org/hardware/IPC-123 onvif://www.onvif.org/location/yard'
  ), '192.168.1.229');
  assert.equal(cam.host, '192.168.1.229');
  assert.equal(cam.deviceUrl, 'http://192.168.1.229:8899/onvif/device_service');
  assert.equal(cam.name, 'Gate Cam');
  assert.equal(cam.hardware, 'IPC-123');
  assert.equal(cam.location, 'yard');
  assert.equal(cam.via, 'ws-discovery');
});

test('the address on the interface that answered wins over a stale one', () => {
  const cam = parseProbeMatch(match('http://10.0.0.5/onvif/device_service http://192.168.1.229/onvif/device_service', ''), '192.168.1.229');
  assert.equal(cam.deviceUrl, 'http://192.168.1.229/onvif/device_service');
});

test('a ProbeMatch with no address falls back to the sender on port 80', () => {
  const cam = parseProbeMatch(match('', ''), '192.168.1.229');
  assert.equal(cam.deviceUrl, 'http://192.168.1.229/onvif/device_service');
  assert.equal(cam.name, '');
});

test('the sweep knocks on the usual ONVIF ports, 80 first', () => {
  assert.equal(ONVIF_PORTS[0], 80);
  for (const port of [8899, 8000, 8080]) assert.ok(ONVIF_PORTS.includes(port));
});

// A stand-in camera: whatever the handler says to /onvif/device_service.
const serve = (status, body) => new Promise((resolve) => {
  const server = http.createServer((req, res) => {
    if (req.url !== '/onvif/device_service') { res.writeHead(404); return res.end('not here'); }
    res.writeHead(status, { 'Content-Type': 'application/soap+xml' });
    res.end(body);
  });
  server.listen(0, '127.0.0.1', () => resolve({ port: server.address().port, close: () => server.close() }));
});

test('a host that answers GetSystemDateAndTime is a confirmed camera', async () => {
  const s = await serve(200, '<s:Envelope><s:Body><tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime/></tds:GetSystemDateAndTimeResponse></s:Body></s:Envelope>');
  try {
    assert.deepEqual(await onvifAt('127.0.0.1', s.port), { deviceUrl: `http://127.0.0.1:${s.port}/onvif/device_service`, confirmed: true });
  } finally { s.close(); }
});

test('a SOAP fault or a 401 still marks an ONVIF service, unconfirmed', async () => {
  const fault = await serve(500, '<s:Envelope><s:Body><s:Fault><s:Reason><s:Text>NotAuthorized</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>');
  const denied = await serve(401, '');
  try {
    assert.equal((await onvifAt('127.0.0.1', fault.port)).confirmed, false);
    assert.equal((await onvifAt('127.0.0.1', denied.port)).confirmed, false);
  } finally { fault.close(); denied.close(); }
});

test('a plain web server is not a camera', async () => {
  const s = await serve(200, '<html><title>router</title></html>');
  try {
    assert.equal(await onvifAt('127.0.0.1', s.port), null);
  } finally { s.close(); }
});

test('reads the reason out of a fault whose prefix has a hyphen in it', () => {
  // What the yard camera (VD-2FT81-ZAS) sends for a wrong password. The
  // SOAP-ENV prefix used to defeat the match and the reason read "SOAP fault".
  const { parseFault } = require('../src/main/services/onvifEventClient');
  const xml = '<?xml version="1.0"?><SOAP-ENV:Envelope><SOAP-ENV:Body><SOAP-ENV:Fault><SOAP-ENV:Code><SOAP-ENV:Value>SOAP-ENV:Sender</SOAP-ENV:Value></SOAP-ENV:Code>'
    + '<SOAP-ENV:Reason><SOAP-ENV:Text xml:lang="en">Sender not Authorized. Invalid username or password!</SOAP-ENV:Text></SOAP-ENV:Reason>'
    + '</SOAP-ENV:Fault></SOAP-ENV:Body></SOAP-ENV:Envelope>';
  assert.equal(parseFault(xml), 'Sender not Authorized. Invalid username or password!');
});

test('reads XAddrs whatever the prefix', () => {
  const xml = '<SOAP-ENV:Envelope><SOAP-ENV:Body><wsdd-1:ProbeMatches><wsdd-1:ProbeMatch><wsdd-1:XAddrs>http://192.168.1.229:8899/onvif/device_service</wsdd-1:XAddrs>'
    + '</wsdd-1:ProbeMatch></wsdd-1:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>';
  assert.equal(parseProbeMatch(xml, '192.168.1.229').deviceUrl, 'http://192.168.1.229:8899/onvif/device_service');
});
