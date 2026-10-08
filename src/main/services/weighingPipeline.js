// What happens when the scale says a truck has settled: take the photo, find
// the truck in it, read its plate off the full-resolution stream, stamp the
// weight and plate on the picture, write it all to the history, and keep
// trying for a better plate while the truck is still on the deck. If the
// scale then settles heavier (the rest of the truck, or its trailer, rolled
// on after the first settle), the same entry is corrected: new weight, new
// picture, the plate kept. The Falcon agent's weighWithoutWatcher, for one
// camera, without a database.
const detector = require('./vehicleDetector');
const plateReader = require('./plateReader');
const { captureRtspSnapshot } = require('./rtspSnapshot');
const { formatWeight } = require('./scaleFrames');

const PLATE_CONFIDENT = 0.9;
const PLATE_CROP_MARGIN = 0.15;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const largestVehicle = (detections) => detections
  .filter((d) => d.isVehicle && !d.tooSmall)
  .sort((a, b) => ((b.box[2] - b.box[0]) * (b.box[3] - b.box[1])) - ((a.box[2] - a.box[0]) * (a.box[3] - a.box[1])))[0] || null;

const oppositeCorner = (corner) => ({
  'top-left': 'top-right', 'top-right': 'top-left', 'bottom-left': 'bottom-right', 'bottom-right': 'bottom-left'
}[corner] || 'bottom-right');

// Crop the vehicle out of a full-resolution frame and read its plate.
const readPlateInBox = async (hdFrame, box) => {
  const sharp = require('sharp');
  const image = sharp(hdFrame, { failOn: 'none' });
  const { width: W, height: H } = await image.metadata();
  if (!W || !H) throw new Error('could not read the high-resolution frame');
  const [x1, y1, x2, y2] = box;
  const mx = (x2 - x1) * PLATE_CROP_MARGIN;
  const my = (y2 - y1) * PLATE_CROP_MARGIN;
  const left = Math.max(0, Math.floor((x1 - mx) * W));
  const top = Math.max(0, Math.floor((y1 - my) * H));
  const right = Math.min(W, Math.ceil((x2 + mx) * W));
  const bottom = Math.min(H, Math.ceil((y2 + my) * H));
  if (right - left < 16 || bottom - top < 16) return null;
  const crop = await image.extract({ left, top, width: right - left, height: bottom - top }).jpeg({ quality: 92 }).toBuffer();
  const { plates } = await plateReader.readPlates(crop);
  if (!plates.length) return null;
  const best = plates.slice().sort((a, b) => b.confidence - a.confidence)[0];
  return { text: best.text, confidence: Math.round(best.confidence * 100) / 100, frameWidth: W, frameHeight: H };
};

class WeighingPipeline {
  /**
   * @param {object} deps
   * @param {() => object} deps.settings   current settings
   * @param {import('./history').History} deps.history
   * @param {() => boolean} deps.isLoaded   is the weighed truck still on the deck
   * @param {(plate) => void} deps.onPlate  a plate read (or improved) for the current weighing
   * @param {(msg) => void} [deps.log]
   */
  constructor({ settings, history, isLoaded, onPlate, log = console.log }) {
    this.settings = settings;
    this.history = history;
    this.isLoaded = isLoaded;
    this.onPlate = onPlate;
    this.log = log;
    this.busy = false;
    this.current = null; // the weighing of the truck now on the deck: { entry, frame, result, target, plate }
  }

  // Stamp the weight and plate on a frame the detector has already looked at.
  async stampPhoto(frame, result, { weightText, plate, capturedAt }) {
    const s = this.settings();
    try {
      return await detector.annotate(frame, {
        ...result,
        zone: [],
        capturedAt,
        plate: plate && s.plates.stampPhoto !== false ? { text: plate.text, corner: s.plates.overlayCorner } : null,
        weight: s.scale.stampPhoto !== false ? { text: weightText, corner: s.scale.overlayCorner || oppositeCorner(s.plates.overlayCorner) } : null
      });
    } catch (err) {
      this.log(`could not annotate the photo: ${err.message}`);
      return frame;
    }
  }

  // The deck emptied: whatever was weighed is done with.
  onCleared() { this.current = null; }

  // The scale settled heavier while the same truck is on the deck: the first
  // settle caught only part of it. The entry keeps its id, time and plate and
  // takes the new weight, with a fresh picture of the whole truck if the
  // camera gives one, or the first picture re-stamped if it does not.
  async onReweighed(weighing) {
    const current = this.current;
    if (!current) return null;
    if (!current.entry) { current.pendingReweigh = weighing; return null; } // still photographing the first settle; applied once written
    const s = this.settings();
    const weightText = formatWeight(weighing.weight, weighing.unit);
    this.log(`${weightText} on the scale - more of the truck; correcting ${formatWeight(weighing.previous, weighing.unit)}`);
    const patch = { weight: weighing.weight, unit: weighing.unit, corrected: { from: weighing.previous, at: new Date(weighing.at).toISOString() } };
    if (!current.frame || !s.camera.streamUrl) return this.history.update(current.entry.id, patch);
    let frame = current.frame;
    let result = current.result;
    let capturedAt = current.capturedAt;
    try {
      frame = await captureRtspSnapshot(s.camera.streamUrl, s.camera.username, s.camera.password, 10000);
      capturedAt = new Date();
      result = { width: 0, height: 0, detections: [] };
      try { result = await detector.detect(frame, { minConfidence: 0.25 }); } catch (err) { this.log(`vehicle detection failed: ${err.message}`); }
      const target = largestVehicle(result.detections);
      if (target) { target.triggered = true; current.target = target; }
      current.frame = frame; current.result = result; current.capturedAt = capturedAt;
    } catch (err) {
      this.log(`could not re-photograph the truck (${err.message}); re-stamping the first picture`);
    }
    const photo = await this.stampPhoto(frame, result, { weightText, plate: current.plate, capturedAt });
    const updated = this.history.update(current.entry.id, patch, photo);
    if (updated) current.entry = updated;
    return updated;
  }

  // A one-off read for the Plates tab: whatever is in front of the camera.
  async readPlateNow() {
    const s = this.settings();
    const hdUrl = s.camera.hdStreamUrl || s.camera.streamUrl;
    if (!hdUrl) throw new Error('No camera stream is set up yet');
    const startedAt = Date.now();
    const frame = await captureRtspSnapshot(s.camera.streamUrl || hdUrl, s.camera.username, s.camera.password, 10000);
    const result = await detector.detect(frame, { minConfidence: 0.25 });
    const vehicles = result.detections.filter((d) => d.isVehicle).slice(0, 6);
    if (!vehicles.length) return { plate: null, error: 'No vehicle in this frame to read a plate from.', image: frame, ms: Date.now() - startedAt };
    const hd = await captureRtspSnapshot(hdUrl, s.camera.username, s.camera.password, 12000);
    let plate = null;
    for (const v of vehicles) {
      const read = await readPlateInBox(hd, v.box);
      if (read && (!plate || read.confidence > plate.confidence)) plate = read;
    }
    return { plate, error: plate ? '' : `No plate found on the ${vehicles.length === 1 ? 'vehicle' : `${vehicles.length} vehicles`}.`, image: frame, detections: result.detections, ms: Date.now() - startedAt };
  }

  async onWeighed(weighing) {
    const s = this.settings();
    if (!s.scale.capturePhoto) return null;
    // Registered before anything is awaited, so a heavier settle that lands
    // while the photo is being taken is held against this weighing.
    const current = { entry: null, frame: null, result: null, target: null, capturedAt: null, plate: null, pendingReweigh: null };
    this.current = current;
    const written = async (entry) => {
      current.entry = entry;
      if (current.pendingReweigh) { const held = current.pendingReweigh; current.pendingReweigh = null; await this.onReweighed(held); }
      return entry;
    };
    const plain = (note) => written(this.history.record({ type: 'weighing', weight: weighing.weight, unit: weighing.unit, at: weighing.at, note }));
    if (!s.camera.streamUrl) { this.log('weighing with no camera set up - logged without a photo'); return plain(''); }
    if (this.busy) { this.log('a weighing is still being photographed; this one is logged without a picture'); return plain('camera busy'); }
    this.busy = true;
    try {
      const weightText = formatWeight(weighing.weight, weighing.unit);
      this.log(`${weightText} on the scale - photographing`);
      const frame = await captureRtspSnapshot(s.camera.streamUrl, s.camera.username, s.camera.password, 10000);
      const capturedAt = new Date();
      let result = { width: 0, height: 0, detections: [] };
      try { result = await detector.detect(frame, { minConfidence: 0.25 }); } catch (err) { this.log(`vehicle detection failed: ${err.message}`); }
      const target = largestVehicle(result.detections);
      if (target) target.triggered = true;
      const wantPlate = !!target && s.plates.enabled && s.scale.readPlate !== false;
      const hdUrl = s.camera.hdStreamUrl || s.camera.streamUrl;
      let plate = null;
      Object.assign(current, { frame, result, target, capturedAt });
      const tryPlate = async (attempt) => {
        try {
          const hd = await captureRtspSnapshot(hdUrl, s.camera.username, s.camera.password, 12000);
          const read = await readPlateInBox(hd, target.box);
          if (!read) { this.log(`no plate found on the ${target.label} (attempt ${attempt})`); return false; }
          if (read.confidence < s.plates.minConfidence) { this.log(`plate "${read.text}" at ${Math.round(read.confidence * 100)}%, under the floor (attempt ${attempt})`); return false; }
          if (!plate || read.confidence > plate.confidence) {
            plate = { text: read.text, confidence: read.confidence, readAt: new Date().toISOString() };
            current.plate = plate;
            this.log(`plate ${plate.text} (${Math.round(plate.confidence * 100)}%, attempt ${attempt})`);
            this.onPlate?.(plate);
            return true;
          }
        } catch (err) {
          this.log(`plate read failed: ${err.message}`);
        }
        return false;
      };
      if (wantPlate) await tryPlate(1);

      const photo = await this.stampPhoto(frame, result, { weightText, plate, capturedAt });
      const entry = this.history.record({
        type: 'weighing', weight: weighing.weight, unit: weighing.unit, at: weighing.at,
        plate, vehicle: target ? { label: target.label, score: target.score } : null
      }, photo);
      await written(entry);

      if (wantPlate) {
        const attempts = Math.max(1, Number(s.plates.retryAttempts) || 10);
        const wait = Math.max(1, Number(s.plates.retrySeconds) || 3) * 1000;
        for (let attempt = 2; attempt <= attempts && (!plate || plate.confidence < PLATE_CONFIDENT); attempt++) {
          await sleep(wait);
          if (!this.isLoaded()) break;
          if (await tryPlate(attempt)) this.history.update(entry.id, { plate });
          if (this.current !== current) break; // the deck cleared, or another truck is being weighed
        }
      }
      return entry;
    } catch (err) {
      this.log(`weighing photo failed: ${err.message}`);
      if (current.entry) return current.entry; // photographed and written; a plate retry failed afterwards
      return plain(`photo failed: ${err.message}`);
    } finally {
      this.busy = false;
    }
  }
}

module.exports = { WeighingPipeline, readPlateInBox, largestVehicle };
