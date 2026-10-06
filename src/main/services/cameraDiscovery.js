// Finding IP cameras on the LAN and asking each for its RTSP streams.
//
// WS-Discovery: a SOAP Probe multicast to 239.255.255.250:3702, which every
// ONVIF camera answers with its device service address. Each answer is then
// asked, with the login the user gave, for its media profiles and the RTSP
// URI of each - the same calls the Falcon backend makes to find a PTZ
// camera's streams, through the same hand-rolled ONVIF client.
const dgram = require('dgram');
const crypto = require('crypto');
const os = require('os');
const { OnvifEventClient } = require('./onvifEventClient');

const MULTICAST = '239.255.255.250';
const PORT = 3702;
const NS_TT = 'http://www.onvif.org/ver10/schema';

const escapeXml = (v) => String(v).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const unescapeXml = (v) => String(v).replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&apos;/g, "'");

const probeXml = () => {
  const id = `urn:uuid:${crypto.randomUUID()}`;
  return '<?xml version="1.0" encoding="UTF-8"?>'
    + '<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:dn="http://www.onvif.org/ver10/network/wsdl">'
    + `<e:Header><w:MessageID>${id}</w:MessageID><w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>`
    + '<w:Action a:mustUnderstand="true" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header>'
    + '<e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>';
};

const localAddresses = () => {
  const out = [];
  for (const list of Object.values(os.networkInterfaces())) {
    for (const iface of list) if (iface.family === 'IPv4' && !iface.internal) out.push(iface.address);
  }
  return out;
};

// Cameras that answer the probe, by device service URL. Sent from every
// interface, since multicast goes out of one at a time.
const probe = (waitMs = 3000) => new Promise((resolve) => {
  const found = new Map();
  const sockets = [];
  const finish = () => {
    for (const s of sockets) { try { s.close(); } catch {} }
    resolve([...found.values()]);
  };
  const addresses = localAddresses();
  if (!addresses.length) return resolve([]);
  let pending = addresses.length;
  for (const address of addresses) {
    const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    sockets.push(socket);
    socket.on('error', () => { pending -= 1; });
    socket.on('message', (msg, rinfo) => {
      const xml = msg.toString('utf8');
      const xaddrs = ((xml.match(/<(?:[A-Za-z0-9_]+:)?XAddrs>([^<]*)</) || [])[1] || '').trim().split(/\s+/).filter(Boolean);
      const scopes = ((xml.match(/<(?:[A-Za-z0-9_]+:)?Scopes>([^<]*)</) || [])[1] || '');
      const scope = (key) => decodeURIComponent(((scopes.match(new RegExp(`onvif://www\\.onvif\\.org/${key}/([^\\s]+)`)) || [])[1] || '').replace(/\+/g, ' '));
      // Prefer the address on the interface that answered; cameras list an
      // IPv6 or a stale address first sometimes.
      const xaddr = xaddrs.find((x) => x.includes(rinfo.address)) || xaddrs[0] || `http://${rinfo.address}/onvif/device_service`;
      found.set(rinfo.address, {
        host: rinfo.address,
        deviceUrl: xaddr,
        name: scope('name') || '',
        hardware: scope('hardware') || '',
        location: scope('location') || ''
      });
    });
    socket.bind(0, address, () => {
      try { socket.setMulticastInterface(address); } catch {}
      const body = Buffer.from(probeXml(), 'utf8');
      // Twice, a moment apart: a single UDP probe on a busy switch is easy to lose.
      socket.send(body, 0, body.length, PORT, MULTICAST, () => {});
      setTimeout(() => { try { socket.send(body, 0, body.length, PORT, MULTICAST, () => {}); } catch {} }, 400);
    });
  }
  setTimeout(finish, waitMs);
  void pending;
});

// The streams one camera offers: profile name, RTSP URI, snapshot URI,
// resolution. Needs the camera's login.
const streamsOf = async ({ deviceUrl, host, username, password }) => {
  const url = deviceUrl || `http://${host}/onvif/device_service`;
  const client = new OnvifEventClient({ deviceUrl: url, username, password });
  await client.connect();
  const profiles = await client._profiles();
  const mediaUrl = await client._mediaUrl();
  const out = [];
  for (const profile of profiles) {
    const body = (tag) =>
      `<trt:Get${tag}Uri xmlns:trt="http://www.onvif.org/ver10/media/wsdl" xmlns:tt="${NS_TT}">`
      + (tag === 'Stream' ? '<trt:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></trt:StreamSetup>' : '')
      + `<trt:ProfileToken>${escapeXml(profile.token)}</trt:ProfileToken></trt:Get${tag}Uri>`;
    const uriOf = async (tag) => {
      try {
        const xml = await client._request(mediaUrl, body(tag));
        return unescapeXml(((xml.match(/<(?:[A-Za-z0-9_]+:)?Uri>([^<]*)</) || [])[1] || '').trim());
      } catch { return ''; }
    };
    out.push({
      profileToken: profile.token,
      profileName: profile.name,
      width: profile.width || null,
      height: profile.height || null,
      streamUri: await uriOf('Stream'),
      snapshotUri: await uriOf('Snapshot')
    });
  }
  return out;
};

module.exports = { probe, streamsOf };
