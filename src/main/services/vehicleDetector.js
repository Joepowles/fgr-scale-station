const fs = require('fs');
const path = require('path');
const https = require('https');
const http = require('http');

// Vehicle detection on a single camera frame using YOLOv10-nano on the CPU.
//
// The model is a ~9 MB ONNX file that is not kept in git: it is downloaded
// once on first use (or by `npm run vehicle-model`) into backend/data.
// YOLOv10 was chosen over v8 because its output is already de-duplicated
// (no NMS step), so post-processing is a straight read of 300 candidate rows.

const paths = require('../paths');
const DEFAULT_MODEL_URL = 'https://huggingface.co/onnx-community/yolov10n/resolve/main/onnx/model.onnx';
const MODEL_URL = process.env.VEHICLE_MODEL_URL || DEFAULT_MODEL_URL;
// Shipped with the app; the download is only a fallback for a build that
// somehow lacks it.
const MODEL_PATH = process.env.VEHICLE_MODEL_PATH || paths.modelPath();
const MIN_MODEL_BYTES = 1024 * 1024;
const INPUT_SIZE = 640;
const THREADS = Math.max(1, Math.min(16, Number(process.env.VEHICLE_DETECTION_THREADS) || 2));

const COCO_CLASSES = [
  'person', 'bicycle', 'car', 'motorcycle', 'airplane', 'bus', 'train', 'truck', 'boat',
  'traffic light', 'fire hydrant', 'stop sign', 'parking meter', 'bench', 'bird', 'cat', 'dog',
  'horse', 'sheep', 'cow', 'elephant', 'bear', 'zebra', 'giraffe', 'backpack', 'umbrella',
  'handbag', 'tie', 'suitcase', 'frisbee', 'skis', 'snowboard', 'sports ball', 'kite',
  'baseball bat', 'baseball glove', 'skateboard', 'surfboard', 'tennis racket', 'bottle',
  'wine glass', 'cup', 'fork', 'knife', 'spoon', 'bowl', 'banana', 'apple', 'sandwich', 'orange',
  'broccoli', 'carrot', 'hot dog', 'pizza', 'donut', 'cake', 'chair', 'couch', 'potted plant',
  'bed', 'dining table', 'toilet', 'tv', 'laptop', 'mouse', 'remote', 'keyboard', 'cell phone',
  'microwave', 'oven', 'toaster', 'sink', 'refrigerator', 'book', 'clock', 'vase', 'scissors',
  'teddy bear', 'hair drier', 'toothbrush'
];

const VEHICLE_CLASSES = new Set(['car', 'truck', 'bus', 'motorcycle']);
// 'person' is COCO class 0, so the model already reports people on every frame
// — they were simply filtered out of alerts until now.
const PERSON_CLASSES = new Set(['person']);

let sessionPromise = null;
let downloadPromise = null;

const download = (url, destination, redirects = 0) => new Promise((resolve, reject) => {
  if (redirects > 5) return reject(new Error('Too many redirects downloading model'));
  const client = url.startsWith('https:') ? https : http;
  const req = client.get(url, { headers: { 'User-Agent': 'fgr-workorders' } }, (res) => {
    if ([301, 302, 303, 307, 308].includes(res.statusCode) && res.headers.location) {
      res.resume();
      const next = new URL(res.headers.location, url).toString();
      return download(next, destination, redirects + 1).then(resolve, reject);
    }
    if (res.statusCode !== 200) {
      res.resume();
      return reject(new Error(`Model download failed: HTTP ${res.statusCode}`));
    }
    const tmp = `${destination}.part`;
    const out = fs.createWriteStream(tmp);
    res.pipe(out);
    out.on('finish', () => {
      out.close(() => {
        try {
          const size = fs.statSync(tmp).size;
          if (size < MIN_MODEL_BYTES) throw new Error(`Downloaded model is too small (${size} bytes)`);
          fs.renameSync(tmp, destination);
          resolve(destination);
        } catch (err) {
          try { fs.unlinkSync(tmp); } catch { /* ignore */ }
          reject(err);
        }
      });
    });
    out.on('error', (err) => { try { fs.unlinkSync(tmp); } catch { /* ignore */ } reject(err); });
  });
  req.on('error', reject);
  req.setTimeout(120000, () => req.destroy(new Error('Model download timed out')));
});

const modelIsPresent = () => {
  try { return fs.statSync(MODEL_PATH).size >= MIN_MODEL_BYTES; } catch { return false; }
};

// Make sure the model file exists, downloading it if needed. Safe to call often.
const ensureModel = async () => {
  if (modelIsPresent()) return MODEL_PATH;
  if (!downloadPromise) {
    fs.mkdirSync(path.dirname(MODEL_PATH), { recursive: true });
    console.log(`[VehicleDetect] Downloading detection model to ${MODEL_PATH} ...`);
    downloadPromise = download(MODEL_URL, MODEL_PATH)
      .then((p) => { console.log('[VehicleDetect] Model ready'); return p; })
      .finally(() => { downloadPromise = null; });
  }
  return downloadPromise;
};

const getSession = async () => {
  if (!sessionPromise) {
    sessionPromise = (async () => {
      await ensureModel();
      const ort = require('onnxruntime-node');
      // Explicit thread counts also stop onnxruntime trying to pin threads to
      // cores, which fails inside an LXC container and spams the log.
      return ort.InferenceSession.create(MODEL_PATH, {
        intraOpNumThreads: THREADS,
        interOpNumThreads: 1,
        graphOptimizationLevel: 'all'
      });
    })().catch((err) => { sessionPromise = null; throw err; });
  }
  return sessionPromise;
};

// Warm the model so the first real detection is not slowed by loading it.
const warmUp = () => getSession();

/**
 * Run the detector on a JPEG frame.
 * Boxes come back normalised to 0..1 of the source image so callers never
 * need to know the frame size the camera happened to send.
 */
const detect = async (jpegBuffer, { minConfidence = 0.4 } = {}) => {
  const sharp = require('sharp');
  const ort = require('onnxruntime-node');
  const session = await getSession();

  const image = sharp(jpegBuffer, { failOn: 'none' });
  const meta = await image.metadata();
  const width = meta.width || 0;
  const height = meta.height || 0;
  if (!width || !height) throw new Error('Could not read frame dimensions');

  // Letterbox into a 640x640 square, keeping aspect ratio.
  const scale = Math.min(INPUT_SIZE / width, INPUT_SIZE / height);
  const scaledW = Math.max(1, Math.round(width * scale));
  const scaledH = Math.max(1, Math.round(height * scale));
  const padX = Math.floor((INPUT_SIZE - scaledW) / 2);
  const padY = Math.floor((INPUT_SIZE - scaledH) / 2);
  const raw = await image
    .resize(scaledW, scaledH)
    .extend({
      top: padY, bottom: INPUT_SIZE - scaledH - padY,
      left: padX, right: INPUT_SIZE - scaledW - padX,
      background: { r: 114, g: 114, b: 114 }
    })
    .removeAlpha()
    .raw()
    .toBuffer();

  const area = INPUT_SIZE * INPUT_SIZE;
  const input = new Float32Array(3 * area);
  for (let i = 0; i < area; i++) {
    input[i] = raw[i * 3] / 255;
    input[area + i] = raw[i * 3 + 1] / 255;
    input[2 * area + i] = raw[i * 3 + 2] / 255;
  }

  const started = Date.now();
  const output = await session.run({ images: new ort.Tensor('float32', input, [1, 3, INPUT_SIZE, INPUT_SIZE]) });
  const result = output[session.outputNames[0]];
  const rows = result.dims[1];
  const data = result.data;

  const clamp01 = (v) => Math.max(0, Math.min(1, v));
  const detections = [];
  for (let i = 0; i < rows; i++) {
    const score = data[i * 6 + 4];
    if (score < minConfidence) continue;
    const classIndex = Math.round(data[i * 6 + 5]);
    const label = COCO_CLASSES[classIndex] || `class ${classIndex}`;
    detections.push({
      label,
      score: Math.round(score * 100) / 100,
      isVehicle: VEHICLE_CLASSES.has(label),
      isPerson: PERSON_CLASSES.has(label),
      box: [
        clamp01((data[i * 6] - padX) / scale / width),
        clamp01((data[i * 6 + 1] - padY) / scale / height),
        clamp01((data[i * 6 + 2] - padX) / scale / width),
        clamp01((data[i * 6 + 3] - padY) / scale / height)
      ]
    });
  }

  return { width, height, detections: dedupeDetections(detections), inferenceMs: Date.now() - started };
};

// The model regularly reports one object under two vehicle classes — the
// Front Lot pickup comes back as "car 51%" AND "truck 34%" with the identical
// box, every frame. Left alone, the second copy looks like a second vehicle to
// the tracker and gets announced as a new arrival. Keep the best-scoring box
// of each overlapping group of the same kind (vehicle with vehicle, person
// with person). Two genuinely separate vehicles rarely overlap this much.
const DUPLICATE_IOU = 0.5;
const dedupeDetections = (detections) => {
  const kept = [];
  for (const d of [...detections].sort((a, b) => b.score - a.score)) {
    const sameKind = (k) => k.isPerson === d.isPerson && k.isVehicle === d.isVehicle;
    const duplicate = kept.some((k) => sameKind(k) && (k.isPerson || k.isVehicle) && iou(k.box, d.box) >= DUPLICATE_IOU);
    if (!duplicate) kept.push(d);
  }
  return kept;
};

// Ray-casting point-in-polygon. Both in normalised 0..1 coordinates.
const pointInPolygon = ([x, y], polygon) => {
  if (!Array.isArray(polygon) || polygon.length < 3) return false;
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const [xi, yi] = polygon[i];
    const [xj, yj] = polygon[j];
    const crosses = ((yi > y) !== (yj > y)) && (x < ((xj - xi) * (y - yi)) / ((yj - yi) || 1e-9) + xi);
    if (crosses) inside = !inside;
  }
  return inside;
};

// A vehicle counts as "in the zone" when the point where it meets the ground
// (bottom-centre of its box) or its centre sits inside the polygon. Using the
// ground point means a zone drawn on the roadway catches trucks whose cab
// towers above it.
const boxInZone = (box, zone) => {
  const [x1, y1, x2, y2] = box;
  const cx = (x1 + x2) / 2;
  return pointInPolygon([cx, y2], zone) || pointInPolygon([cx, (y1 + y2) / 2], zone);
};

const iou = (a, b) => {
  const ix = Math.max(0, Math.min(a[2], b[2]) - Math.max(a[0], b[0]));
  const iy = Math.max(0, Math.min(a[3], b[3]) - Math.max(a[1], b[1]));
  const inter = ix * iy;
  const union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
  return union > 0 ? inter / union : 0;
};

// How much of the SMALLER box lies inside the larger one. IoU punishes a pair
// of boxes for differing in size, which is the wrong question when asking "is
// this the same parked car as last time": the model's box for one stationary
// vehicle routinely shrinks to the half of it that is not in shadow or behind
// a passer-by. Two cars parked side by side overlap far less by this measure
// than one car does with a shrunken box of itself.
const containment = (a, b) => {
  const ix = Math.max(0, Math.min(a[2], b[2]) - Math.max(a[0], b[0]));
  const iy = Math.max(0, Math.min(a[3], b[3]) - Math.max(a[1], b[1]));
  const inter = ix * iy;
  const smaller = Math.min((a[2] - a[0]) * (a[3] - a[1]), (b[2] - b[0]) * (b[3] - b[1]));
  return smaller > 0 ? inter / smaller : 0;
};

// The plate, big and plain, in the corner the user chose. Bold white on
// black with a white border: the one thing on the picture that has to be
// readable at a glance from across the room, and from a phone notification.
// stack: how many chips already sit in this corner, so a second one (the
// weight under the plate) is drawn beside it rather than on top of it.
const plateChipSvg = ({ width, height, text, corner = 'bottom-left', stack = 0 }) => {
  const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;');
  const fontSize = Math.max(24, Math.round(width / 22));
  const padX = Math.round(fontSize * 0.45);
  const padY = Math.round(fontSize * 0.25);
  const border = Math.max(2, Math.round(fontSize / 12));
  // Monospace-ish estimate per character, plus the letter spacing below.
  const spacing = Math.round(fontSize * 0.08);
  const boxWidth = Math.round(text.length * (fontSize * 0.68 + spacing)) + padX * 2;
  const boxHeight = fontSize + padY * 2;
  const margin = Math.max(8, Math.round(width / 80));
  // Bottom right already carries the server clock, so there the chip sits
  // above it.
  const clockClearance = corner === 'bottom-right' ? Math.round(width / 70) * 2 + margin : 0;
  const shift = stack * (boxHeight + Math.round(margin / 2));
  const x = /right$/.test(corner) ? Math.max(0, width - boxWidth - margin) : margin;
  const y = /^bottom/.test(corner) ? Math.max(0, height - boxHeight - margin - clockClearance - shift) : margin + shift;
  return `<rect x="${x}" y="${y}" width="${boxWidth}" height="${boxHeight}" rx="${Math.round(fontSize / 6)}" fill="#0b0b0b" stroke="#ffffff" stroke-width="${border}"/>`
    + `<text x="${x + padX}" y="${y + padY + fontSize - Math.round(fontSize * 0.18)}"`
    + ` font-family="DejaVu Sans Mono, Liberation Mono, monospace" font-weight="bold" font-size="${fontSize}" letter-spacing="${spacing}" fill="#ffffff">${esc(text)}</text>`;
};

// Draw the zone and boxes onto the frame for the access log snapshot.
// weight is the scale reading at the moment of the photo, { text, corner },
// drawn the same way as the plate so the two read as a pair.
const annotate = async (jpegBuffer, { width, height, detections = [], zone = [], capturedAt, plate = null, weight = null } = {}) => {
  const sharp = require('sharp');
  const { timestampSvg } = require('./imageTimestamp');
  const px = (v, size) => Math.round(v * size);
  const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;');
  let svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}">`;
  if (zone.length >= 3) {
    const points = zone.map(([x, y]) => `${px(x, width)},${px(y, height)}`).join(' ');
    svg += `<polygon points="${points}" fill="rgba(59,130,246,0.15)" stroke="#3b82f6" stroke-width="2"/>`;
  }
  const stroke = Math.max(2, Math.round(width / 400));
  const fontSize = Math.max(12, Math.round(width / 50));
  for (const d of detections) {
    const [x1, y1, x2, y2] = d.box;
    // isSubject is set by the caller once it knows which subjects are watched;
    // fall back to vehicles so a bare call still annotates as it used to.
    const watched = d.isSubject !== undefined ? d.isSubject : d.isVehicle;
    const alerting = d.qualifies !== undefined ? d.qualifies : d.inZone;
    // Red is the subject this alert is about - the one that was not there
    // before. Green is a subject that qualifies but was already reported and
    // has simply been sitting there since. Amber is watched but not alerting
    // (outside the zone, or under the confidence threshold), grey is something
    // not being watched for at all.
    const color = d.triggered ? '#ef4444'
      : alerting ? '#22c55e'
        : watched ? '#f59e0b' : '#9ca3af';
    const label = `${d.label} ${Math.round(d.score * 100)}%`;
    const bx = px(x1, width);
    const by = px(y1, height);
    svg += `<rect x="${bx}" y="${by}" width="${px(x2 - x1, width)}" height="${px(y2 - y1, height)}" fill="none" stroke="${color}" stroke-width="${d.triggered ? stroke * 2 : stroke}"/>`;
    svg += `<rect x="${bx}" y="${Math.max(0, by - fontSize - 4)}" width="${label.length * fontSize * 0.6 + 8}" height="${fontSize + 4}" fill="${color}"/>`;
    svg += `<text x="${bx + 4}" y="${Math.max(fontSize, by - 4)}" font-family="sans-serif" font-size="${fontSize}" fill="#111">${esc(label)}</text>`;
  }
  // Our own clock, last so it sits above the boxes if anything overlaps it.
  svg += timestampSvg({ width, height, when: capturedAt });
  if (plate?.text) svg += plateChipSvg({ width, height, text: plate.text, corner: plate.corner });
  if (weight?.text) svg += plateChipSvg({ width, height, text: weight.text, corner: weight.corner, stack: plate?.text && plate.corner === weight.corner ? 1 : 0 });
  svg += '</svg>';
  return sharp(jpegBuffer, { failOn: 'none' })
    .composite([{ input: Buffer.from(svg), top: 0, left: 0 }])
    .jpeg({ quality: 85 })
    .toBuffer();
};

module.exports = {
  MODEL_PATH,
  VEHICLE_CLASSES,
  PERSON_CLASSES,
  dedupeDetections,
  ensureModel,
  modelIsPresent,
  warmUp,
  detect,
  pointInPolygon,
  boxInZone,
  iou,
  containment,
  annotate,
  plateChipSvg
};
