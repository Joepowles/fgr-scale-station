// Finding IP cameras on the LAN and asking each for its RTSP streams.
//
// Two ways, both used on every network this PC sits on:
//
// WS-Discovery: a SOAP Probe multicast to 239.255.255.250:3702, which every
// ONVIF camera answers with its device service address. Sent from each
// interface in turn, since multicast leaves by one at a time. Quick, but it
// needs the camera to have discovery turned on and Windows to let the
// camera's reply back in - the reply comes from the camera's own address,
// not the multicast one, and the firewall drops it on a "public" network.
//
// Sweep: the same Probe sent straight to every address in each /24, and a
// knock on the ports ONVIF is usually served on, with an unauthenticated
// GetSystemDateAndTime to whatever answers. Slower (a few seconds a network)
// but it finds a camera whether or not multicast works.
//
// Each camera found is then asked, with the login the user gave, for its
// media profiles and the RTSP URI of each - the same calls the Falcon backend
// makes to find a PTZ camera's streams, through the same hand-rolled ONVIF
// client.
const dgram = require('dgram');
const crypto = require('crypto');
const os = require('os');
const { OnvifEventClient } = require('./onvifEventClient');
const { localSubnets, tcpOpen } = require('./nportDiscovery');

const MULTICAST = '239.255.255.250';
const PORT = 3702;
const NS_TT = 'http://www.onvif.org/ver10/schema';
const NS_TDS = 'http://www.onvif.org/ver10/device/wsdl';

// Where cameras serve ONVIF: 80 by the spec, 8899 on Xiongmai boards, 8000
// on Hikvision and Reolink, 8080 on a few others, 2020 on TP-Link.
const ONVIF_PORTS = [80, 8899, 8000, 8080, 2020];

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

// What a ProbeMatch says about the camera that sent it.
const parseProbeMatch = (xml, fromAddress) => {
  const xaddrs = ((xml.match(/<(?:[A-Za-z0-9_.-]+:)?XAddrs>([^<]*)</) || [])[1] || '').trim().split(/\s+/).filter(Boolean);
  const scopes = ((xml.match(/<(?:[A-Za-z0-9_.-]+:)?Scopes>([^<]*)</) || [])[1] || '');
  const scope = (key) => decodeURIComponent(((scopes.match(new RegExp(`onvif://www\\.onvif\\.org/${key}/([^\\s]+)`)) || [])[1] || '').replace(/\+/g, ' '));
  // Prefer the address on the interface that answered; cameras list an
  // IPv6 or a stale address first sometimes.
  const xaddr = xaddrs.find((x) => x.includes(fromAddress)) || xaddrs[0] || `http://${fromAddress}/onvif/device_service`;
  return {
    host: fromAddress,
    deviceUrl: xaddr,
    name: scope('name') || '',
    hardware: scope('hardware') || '',
    location: scope('location') || '',
    via: 'ws-discovery'
  };
};

// Cameras that answer the probe, by address. One socket per interface: the
// multicast goes out of it, and the same probe is sent straight to every
// address in that interface's /24, which a camera answers even when the
// multicast never reached it.
const probe = (waitMs = 3000, { subnets = localSubnets() } = {}) => new Promise((resolve) => {
  const found = new Map();
  const sockets = [];
  const finish = () => {
    for (const s of sockets) { try { s.close(); } catch {} }
    resolve([...found.values()]);
  };
  const addresses = localAddresses();
  if (!addresses.length) return resolve([]);
  for (const address of addresses) {
    const socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
    sockets.push(socket);
    socket.on('error', () => {});
    socket.on('message', (msg, rinfo) => {
      const xml = msg.toString('utf8');
      if (!/ProbeMatch/i.test(xml)) return;
      found.set(rinfo.address, parseProbeMatch(xml, rinfo.address));
    });
    socket.bind(0, address, () => {
      try { socket.setMulticastInterface(address); } catch {}
      const body = Buffer.from(probeXml(), 'utf8');
      const sendTo = (host) => { try { socket.send(body, 0, body.length, PORT, host, () => {}); } catch {} };
      // Twice, a moment apart: a single UDP probe on a busy switch is easy to lose.
      sendTo(MULTICAST);
      setTimeout(() => sendTo(MULTICAST), 400);
      const subnet = subnets.find((s) => s.self === address);
      if (subnet) {
        for (let i = 1; i <= 254; i++) {
          const host = `${subnet.base}.${i}`;
          if (host !== address) sendTo(host);
        }
      }
    });
  }
  setTimeout(finish, waitMs);
});

// Does this host:port serve ONVIF? Asked for the time, which needs no login.
// A SOAP answer of any kind - even a fault or a 401 - means an ONVIF service
// is there; a plain web server gives 404 for the path.
const onvifAt = async (host, port) => {
  const deviceUrl = `http://${host}:${port}/onvif/device_service`;
  const client = new OnvifEventClient({ deviceUrl });
  try {
    const xml = await client._request(deviceUrl, `<tds:GetSystemDateAndTime xmlns:tds="${NS_TDS}"/>`);
    return /SystemDateAndTime/i.test(xml) ? { deviceUrl, confirmed: true } : null;
  } catch (err) {
    const m = String(err.message || '');
    if (/^ONVIF fault/.test(m) || /ONVIF HTTP 401/.test(m)) return { deviceUrl, confirmed: false };
    return null;
  }
};

// The make and model, when the camera will say without a login (many will).
const describe = async (deviceUrl, { username, password } = {}) => {
  const client = new OnvifEventClient({ deviceUrl, username, password });
  try {
    const xml = await client._request(deviceUrl, `<tds:GetDeviceInformation xmlns:tds="${NS_TDS}"/>`);
    const field = (name) => unescapeXml(((xml.match(new RegExp(`<(?:[A-Za-z0-9_.-]+:)?${name}>([^<]*)<`)) || [])[1] || '').trim());
    return { name: field('Model'), hardware: [field('Manufacturer'), field('HardwareId')].filter(Boolean).join(' ') };
  } catch { return { name: '', hardware: '' }; }
};

// Every address in a /24, knocked on the ONVIF ports. Forty hosts at a time,
// all their ports at once: a network with nothing on it takes about five
// seconds.
const sweepSubnet = async (subnet, { ports, onProgress, skip } = {}) => {
  const hits = [];
  const hosts = [];
  for (let i = 1; i <= 254; i++) hosts.push(`${subnet.base}.${i}`);
  const batch = 40;
  for (let i = 0; i < hosts.length; i += batch) {
    const slice = hosts.slice(i, i + batch);
    await Promise.all(slice.map(async (host) => {
      if (host === subnet.self || skip?.has(host)) return;
      const open = (await Promise.all(ports.map(async (port) => ((await tcpOpen(host, port)) ? port : null)))).filter((p) => p !== null);
      for (const port of open) {
        const found = await onvifAt(host, port);
        if (found) { hits.push({ host, ...found }); break; }
      }
    }));
    onProgress?.(Math.min(hosts.length, i + batch), hosts.length, subnet.base);
  }
  return hits;
};

// Everything that looks like a camera on every network this PC is on, and
// the list of networks searched so the user can see the right one was.
const discover = async ({ username, password, onvifPort, onProgress } = {}) => {
  const subnets = localSubnets();
  const ports = [...new Set([...ONVIF_PORTS, Number(onvifPort) || 80])];
  const byHost = new Map();
  for (const cam of await probe(3500, { subnets })) byHost.set(cam.host, cam);
  for (const subnet of subnets) {
    for (const hit of await sweepSubnet(subnet, { ports, onProgress, skip: new Set(byHost.keys()) })) {
      byHost.set(hit.host, { host: hit.host, deviceUrl: hit.deviceUrl, name: '', hardware: '', location: '', via: 'scan', confirmed: hit.confirmed });
    }
  }
  const cameras = [];
  for (const cam of byHost.values()) {
    if (!cam.name && !cam.hardware) Object.assign(cam, await describe(cam.deviceUrl, { username, password }));
    cameras.push(cam);
  }
  cameras.sort((a, b) => a.host.localeCompare(b.host, undefined, { numeric: true }));
  return { cameras, networks: subnets.map((s) => `${s.base}.0/24`) };
};

// The streams one camera offers: profile name, RTSP URI, snapshot URI,
// resolution. Needs the camera's login.
const streamsOf = async ({ deviceUrl, host, port, username, password }) => {
  const url = deviceUrl || `http://${host}:${Number(port) || 80}/onvif/device_service`;
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
        return unescapeXml(((xml.match(/<(?:[A-Za-z0-9_.-]+:)?Uri>([^<]*)</) || [])[1] || '').trim());
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

module.exports = { probe, discover, streamsOf, parseProbeMatch, onvifAt, ONVIF_PORTS };
