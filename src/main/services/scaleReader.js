// The station's reader for one truck scale: a TCP connection to the serial
// device server, the frames parsed into readings, a settled weight over the
// threshold raised as a weighing. The Falcon backend's scaleReaderService,
// less the database: status lives in memory and goes to the window over IPC.
const net = require('net');
const { EventEmitter } = require('events');
const { FrameSplitter, WeighingTrigger } = require('./scaleFrames');

const RECONNECT_MIN_MS = 2000;
const RECONNECT_MAX_MS = 60000;
const SILENCE_TIMEOUT_MS = 15000;
const STATUS_MIN_INTERVAL_MS = 250;

class ScaleReader extends EventEmitter {
  constructor(settings) {
    super();
    this.settings = settings;
    this.socket = null;
    this.stopped = false;
    this.reconnectMs = RECONNECT_MIN_MS;
    this.reconnectTimer = null;
    this.silenceTimer = null;
    this.splitter = new FrameSplitter({ resync: true });
    this.trigger = new WeighingTrigger(settings);
    this.frames = 0;
    this.lastEmitAt = 0;
    this.status = {
      connected: false, weight: null, unit: settings.unit || 'lb', settled: false, loaded: false,
      motion: false, frames: 0, lastFrameAt: null, lastWeighing: null, lastError: '', lastErrorAt: null, updatedAt: Date.now()
    };
  }

  start() { this.connect(); return this; }

  stop() {
    this.stopped = true;
    clearTimeout(this.reconnectTimer);
    clearTimeout(this.silenceTimer);
    if (this.socket) { this.socket.destroy(); this.socket = null; }
    this.setStatus({ connected: false }, true);
  }

  connect() {
    if (this.stopped) return;
    const socket = net.createConnection({ host: this.settings.host, port: this.settings.port });
    this.socket = socket;
    this.splitter = new FrameSplitter({ resync: true });
    socket.setNoDelay(true);
    socket.setKeepAlive(true, 10000);
    socket.once('connect', () => {
      this.reconnectMs = RECONNECT_MIN_MS;
      this.setStatus({ connected: true, lastError: '' }, true);
      this.armSilenceTimer();
    });
    socket.on('data', (chunk) => this.onData(chunk));
    socket.on('error', (err) => this.setStatus({ lastError: err.message, lastErrorAt: Date.now() }, true));
    socket.on('close', () => {
      if (this.socket === socket) this.socket = null;
      clearTimeout(this.silenceTimer);
      this.setStatus({ connected: false, settled: false }, true);
      this.scheduleReconnect();
    });
  }

  scheduleReconnect() {
    if (this.stopped) return;
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = setTimeout(() => this.connect(), this.reconnectMs);
    this.reconnectMs = Math.min(RECONNECT_MAX_MS, this.reconnectMs * 2);
  }

  armSilenceTimer() {
    clearTimeout(this.silenceTimer);
    this.silenceTimer = setTimeout(() => {
      this.setStatus({ lastError: 'No data from the scale', lastErrorAt: Date.now() }, true);
      if (this.socket) this.socket.destroy();
    }, SILENCE_TIMEOUT_MS);
  }

  onData(chunk) {
    this.armSilenceTimer();
    const now = Date.now();
    for (const reading of this.splitter.feed(chunk)) {
      this.frames += 1;
      const event = this.trigger.push(reading, now);
      this.setStatus({
        weight: reading.weight,
        unit: reading.unit || this.settings.unit || 'lb',
        settled: this.trigger.settled,
        loaded: !this.trigger.armed,
        motion: reading.motion,
        frames: this.frames,
        lastFrameAt: now
      });
      if (!event) continue;
      if (event.type === 'weighed') {
        const weighing = { weight: event.weight, unit: event.unit || this.settings.unit || 'lb', gross: event.gross, at: now, plate: null };
        this.setStatus({ lastWeighing: weighing }, true);
        this.emit('weighed', weighing);
      } else if (event.type === 'reweighed') {
        // More of the same truck on the deck: the weighing is corrected, its plate kept.
        const weighing = { ...this.status.lastWeighing, weight: event.weight, unit: event.unit || this.settings.unit || 'lb', gross: event.gross, at: now, previous: event.previous };
        this.setStatus({ lastWeighing: weighing }, true);
        this.emit('reweighed', weighing);
      } else if (event.type === 'cleared') {
        this.setStatus({ loaded: false }, true);
        this.emit('cleared', { weight: event.weight, at: now });
      }
    }
  }

  // The plate read off the weighed truck, kept with the weighing so the
  // overlay can show it while the truck is still on the deck.
  recordPlate(plate) {
    if (!this.status.lastWeighing) return;
    this.setStatus({ lastWeighing: { ...this.status.lastWeighing, plate } }, true);
  }

  get loaded() { return !this.trigger.armed; }

  // Twenty frames a second is more than a window needs; the status goes out
  // at most four times a second unless something notable happened.
  setStatus(patch, force = false) {
    Object.assign(this.status, patch, { updatedAt: Date.now() });
    if (!force && Date.now() - this.lastEmitAt < STATUS_MIN_INTERVAL_MS) return;
    this.lastEmitAt = Date.now();
    this.emit('status', { ...this.status });
  }
}

// For the Scale tab: listen for a few seconds with the address as typed.
const testConnection = ({ host, port = 950, seconds = 3 } = {}) => new Promise((resolve) => {
  const target = String(host || '').trim();
  if (!target) return resolve({ ok: false, error: 'Enter the device server address first' });
  const splitter = new FrameSplitter({ resync: true });
  const readings = [];
  let raw = Buffer.alloc(0);
  let error = '';
  let connected = false;
  const startedAt = Date.now();
  const socket = net.createConnection({ host: target, port: Number(port) || 950 });
  const finish = () => {
    clearTimeout(timer);
    socket.destroy();
    const weights = readings.map((r) => r.weight);
    resolve({
      ok: connected && !error && readings.length > 0,
      connected,
      error: error || (connected && !readings.length ? 'Connected, but nothing arrived. Is this the data port, and is the indicator in continuous output mode?' : ''),
      frames: readings.length,
      durationMs: Date.now() - startedAt,
      framesPerSecond: readings.length ? Math.round(readings.length / Math.max(0.5, (Date.now() - startedAt) / 1000)) : 0,
      latest: readings.length ? readings[readings.length - 1] : null,
      min: weights.length ? Math.min(...weights) : null,
      max: weights.length ? Math.max(...weights) : null,
      // eslint-disable-next-line no-control-regex
      sample: raw.subarray(0, 120).toString('latin1').replace(/[\x00-\x1f]/g, (c) => `<${c.charCodeAt(0).toString(16).padStart(2, '0')}>`)
    });
  };
  const timer = setTimeout(finish, Math.max(1, Math.min(10, Number(seconds) || 3)) * 1000);
  socket.setTimeout(5000, () => { if (!connected) { error = 'Connection timed out'; finish(); } });
  socket.once('connect', () => { connected = true; });
  socket.on('data', (chunk) => {
    if (raw.length < 120) raw = Buffer.concat([raw, chunk]);
    readings.push(...splitter.feed(chunk));
    if (readings.length >= 200) finish();
  });
  socket.on('error', (err) => { error = err.message; finish(); });
});

module.exports = { ScaleReader, testConnection };
