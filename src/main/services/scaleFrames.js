// Reading a truck scale's continuous serial output, and deciding when a
// weight on it is "a vehicle, settled, worth a photo".
//
// Pure: no sockets, no clock of its own. services/scaleReaderService.js feeds
// bytes in and timestamps readings; tests/scaleFrames.test.js drives it with
// captured frames. Kept apart from the socket code so the format quirks of the
// next indicator are a change here and nowhere else.
//
// The yard's indicator sends, twenty times a second, through the NPort:
//
//     <STX>    140LG <CR><LF>
//
// a seven-character weight, a unit letter (L = lb, K = kg), G for gross or N
// for net, then a status character: space when the reading is good and still,
// a letter when it is not (M = motion, O = over range, I = invalid on the
// common Rice Lake / Cardinal formats). Other indicators drop the STX, send
// "lb" in full, or put the status first; the parser is loose enough for those
// without being told.

const STX = 0x02;

// One line (or STX-delimited frame) in, one reading or null out.
const parseFrame = (text) => {
  // eslint-disable-next-line no-control-regex
  const raw = String(text || '').replace(/[\x02\x03\r\n]/g, '');
  const trimmed = raw.trim();
  if (!trimmed) return null;
  // The first number on the line is the weight. Anything before it that is a
  // letter is a status/mode prefix on some indicators; the same letters after
  // it are the common case.
  const m = trimmed.match(/^([A-Za-z\s]*?)([+-]?)\s*(\d+(?:\.\d+)?)\s*(lbs?|kgs?|L|K)?\s*(.*)$/i);
  if (!m) return null;
  const prefix = m[1] || '';
  const value = Number(m[3]) * (m[2] === '-' ? -1 : 1);
  if (!Number.isFinite(value)) return null;
  const unitLetter = (m[4] || '').toLowerCase();
  const unit = unitLetter.startsWith('k') ? 'kg' : unitLetter ? 'lb' : null;
  const flags = `${prefix}${m[5] || ''}`.toUpperCase();
  // G/N say gross or net; the status letters say whether to trust the number.
  const gross = !/\bN\b|NET/.test(flags);
  const motion = /M/.test(flags.replace(/NET|GR/g, ''));
  const invalid = /[OI]/.test(flags.replace(/NET|GR/g, ''));
  return { weight: value, unit, gross, motion, valid: !invalid, raw };
};

// Turns a byte stream into readings, however the chunks fall. A frame ends at
// LF, or at the next STX for indicators that send no line ending.
class FrameSplitter {
  // A connection opened mid-stream starts partway through a frame: "0LG "
  // is the tail of "    140LG ", and reads as a weight of zero. With resync
  // the first frame boundary is where reading starts.
  constructor({ resync = false } = {}) { this.pending = ''; this.resync = resync; }

  feed(chunk) {
    const text = Buffer.isBuffer(chunk) ? chunk.toString('latin1') : String(chunk);
    this.pending += text;
    const readings = [];
    // Cut at every LF and at every STX that is not at the very start.
    let cut;
    while ((cut = this.nextCut()) !== -1) {
      const frame = this.pending.slice(0, cut);
      this.pending = this.pending.slice(cut + 1);
      if (this.resync) { this.resync = false; continue; }
      const reading = parseFrame(frame);
      if (reading) readings.push(reading);
    }
    // Nothing sensible is longer than this: a stuck line without terminators
    // would otherwise grow forever.
    if (this.pending.length > 256) this.pending = this.pending.slice(-64);
    return readings;
  }

  nextCut() {
    const lf = this.pending.indexOf('\n');
    const stx = this.pending.indexOf(String.fromCharCode(STX), 1);
    if (lf === -1 && stx === -1) return -1;
    if (lf === -1) return stx - 1 >= 0 ? stx - 1 : -1;
    if (stx === -1 || lf < stx) return lf;
    // An STX before the next LF starts a new frame: the bytes before it are
    // a frame of their own (cut just before the STX so it stays with the next).
    this.pending = `${this.pending.slice(0, stx)}\n${this.pending.slice(stx)}`;
    return stx;
  }
}

// When does a stream of readings amount to "a vehicle is on the deck"?
//
// A truck rolling on takes a second or two to settle, and the indicator's own
// motion flag lifts a moment before it has really stopped rocking. So the
// weight has to sit inside a tolerance band for stableSeconds, with no motion
// flag anywhere in that window, and be at or above minWeight. That fires once.
// It does not fire again until the deck has gone back under clearWeight: a
// truck shuffling forward a few feet is the same truck.
class WeighingTrigger {
  constructor({ minWeight = 5000, clearWeight = 1000, stableSeconds = 2, stableTolerance = 40 } = {}) {
    this.minWeight = Number(minWeight) || 0;
    this.clearWeight = Math.min(Number(clearWeight) || 0, this.minWeight);
    this.stableMs = Math.max(500, (Number(stableSeconds) || 2) * 1000);
    this.tolerance = Math.max(0, Number(stableTolerance) || 0);
    this.window = []; // [{ at, weight, motion }]
    this.armed = true;
    this.settled = false;
    this.latest = null;
    this.lastWeighing = null;
  }

  push(reading, at = Date.now()) {
    if (!reading || !reading.valid) return null;
    this.latest = { ...reading, at };
    this.window.push({ at, weight: reading.weight, motion: reading.motion });
    const since = at - this.stableMs;
    while (this.window.length && this.window[0].at < since) this.window.shift();

    const spansWindow = this.window.length >= 2 && (at - this.window[0].at) >= this.stableMs * 0.9;
    let settled = false;
    if (spansWindow) {
      let min = Infinity; let max = -Infinity; let moving = false;
      for (const r of this.window) {
        if (r.weight < min) min = r.weight;
        if (r.weight > max) max = r.weight;
        if (r.motion) moving = true;
      }
      settled = !moving && (max - min) <= this.tolerance;
    }
    this.settled = settled;

    if (!this.armed) {
      if (reading.weight <= this.clearWeight) {
        this.armed = true;
        return { type: 'cleared', weight: reading.weight, at };
      }
      return null;
    }
    if (settled && reading.weight >= this.minWeight) {
      this.armed = false;
      this.lastWeighing = { weight: reading.weight, unit: reading.unit, at };
      return { type: 'weighed', weight: reading.weight, unit: reading.unit, gross: reading.gross, at };
    }
    return null;
  }
}

// "48,320 lb" - the one way a weight is written everywhere it appears.
const formatWeight = (value, unit = 'lb') => {
  const n = Number(value);
  if (!Number.isFinite(n)) return '';
  const rounded = Math.abs(n) >= 100 ? Math.round(n) : Math.round(n * 10) / 10;
  return `${rounded.toLocaleString('en-US')} ${unit === 'kg' ? 'kg' : 'lb'}`;
};

module.exports = { parseFrame, FrameSplitter, WeighingTrigger, formatWeight, STX };
