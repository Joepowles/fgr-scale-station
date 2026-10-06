// Finding the serial device server the scale is wired to.
//
// Two ways, both used: the Moxa NPort answers a UDP broadcast on port 4800
// (the protocol its own "NPort Search" utility speaks), and failing that the
// LAN is scanned for the TCP ports an NPort serves serial data on - 950 in
// Real COM mode, 4001 in TCP Server mode - with the device's web console
// checked for the "NPort" name. Anything that answers is listed with the port
// that carried data, so the Scale tab can offer it with one click.
const dgram = require('dgram');
const net = require('net');
const http = require('http');
const os = require('os');

const DATA_PORTS = [950, 4001];
const MOXA_SEARCH_PORT = 4800;

// Every IPv4 /24 this machine sits on. Larger subnets are cut to the /24 the
// machine's own address is in: a yard LAN is one of those.
const localSubnets = () => {
  const out = [];
  for (const list of Object.values(os.networkInterfaces())) {
    for (const iface of list) {
      if (iface.family !== 'IPv4' || iface.internal) continue;
      const base = iface.address.split('.').slice(0, 3).join('.');
      if (!out.some((s) => s.base === base)) out.push({ base, self: iface.address, broadcast: `${base}.255` });
    }
  }
  return out;
};

const tcpOpen = (host, port, timeoutMs = 700) => new Promise((resolve) => {
  const socket = net.createConnection({ host, port });
  const done = (ok) => { socket.destroy(); resolve(ok); };
  socket.setTimeout(timeoutMs, () => done(false));
  socket.once('connect', () => done(true));
  socket.once('error', () => done(false));
});

// The NPort's web console title, when it has one, says what the box is.
const webTitle = (host) => new Promise((resolve) => {
  const req = http.get({ host, port: 80, path: '/', timeout: 1500 }, (res) => {
    let data = '';
    res.setEncoding('utf8');
    res.on('data', (c) => { if (data.length < 4096) data += c; });
    res.on('end', () => resolve((data.match(/<title>([^<]*)<\/title>/i) || [])[1]?.trim() || ''));
  });
  req.on('timeout', () => { req.destroy(); resolve(''); });
  req.on('error', () => resolve(''));
});

// Moxa's search protocol: a 24-byte broadcast, replies carry the model name
// and IP. The format is not published; what is parsed here is what an NPort
// 5110 answers with (an ASCII model string and the dotted IP in the reply).
const moxaBroadcast = (subnets, waitMs = 1500) => new Promise((resolve) => {
  const found = new Map();
  let socket;
  try {
    socket = dgram.createSocket({ type: 'udp4', reuseAddr: true });
  } catch { return resolve([]); }
  socket.on('error', () => { try { socket.close(); } catch {} resolve([...found.values()]); });
  socket.on('message', (msg, rinfo) => {
    const text = msg.toString('latin1');
    const model = (text.match(/NPort[\s-]?[0-9A-Za-z-]+/) || [])[0] || 'NPort';
    found.set(rinfo.address, { host: rinfo.address, model, via: 'moxa-search' });
  });
  socket.bind(0, () => {
    socket.setBroadcast(true);
    const probe = Buffer.alloc(24);
    probe.writeUInt32BE(0x01000008, 0);
    for (const s of subnets) {
      try { socket.send(probe, 0, probe.length, MOXA_SEARCH_PORT, s.broadcast); } catch {}
    }
    try { socket.send(probe, 0, probe.length, MOXA_SEARCH_PORT, '255.255.255.255'); } catch {}
    setTimeout(() => { try { socket.close(); } catch {} resolve([...found.values()]); }, waitMs);
  });
});

const scanSubnet = async (subnet, { onProgress } = {}) => {
  const hits = [];
  const hosts = [];
  for (let i = 1; i <= 254; i++) hosts.push(`${subnet.base}.${i}`);
  // Sixty at a time keeps the scan to a few seconds without flooding a small
  // switch with hundreds of SYNs at once.
  const batch = 60;
  for (let i = 0; i < hosts.length; i += batch) {
    const slice = hosts.slice(i, i + batch);
    await Promise.all(slice.map(async (host) => {
      if (host === subnet.self) return;
      for (const port of DATA_PORTS) {
        if (await tcpOpen(host, port)) { hits.push({ host, port }); break; }
      }
    }));
    onProgress?.(Math.min(hosts.length, i + batch), hosts.length);
  }
  return hits;
};

// Everything on the LAN that looks like a serial device server, best guess
// first: a Moxa search reply or an "NPort" web title beats a bare open port.
const discover = async ({ onProgress } = {}) => {
  const subnets = localSubnets();
  const byHost = new Map();
  for (const hit of await moxaBroadcast(subnets)) byHost.set(hit.host, { ...hit, port: null });
  for (const subnet of subnets) {
    for (const hit of await scanSubnet(subnet, { onProgress })) {
      const existing = byHost.get(hit.host) || { host: hit.host, via: 'port-scan' };
      byHost.set(hit.host, { ...existing, port: hit.port });
    }
  }
  const results = [];
  for (const entry of byHost.values()) {
    const title = await webTitle(entry.host);
    const isNport = /nport/i.test(title) || entry.via === 'moxa-search';
    if (entry.port === null) {
      // Found by broadcast but no data port answered: still worth listing.
      for (const port of DATA_PORTS) { if (await tcpOpen(entry.host, port)) { entry.port = port; break; } }
    }
    results.push({
      host: entry.host,
      port: entry.port || 950,
      model: entry.model || (isNport ? 'NPort' : ''),
      title,
      likely: isNport,
      // In Real COM mode the data port is 950; 4001 means TCP Server mode.
      mode: entry.port === 4001 ? 'TCP Server' : entry.port === 950 ? 'Real COM' : ''
    });
  }
  return results.sort((a, b) => Number(b.likely) - Number(a.likely) || a.host.localeCompare(b.host));
};

module.exports = { discover, localSubnets, tcpOpen, DATA_PORTS };
