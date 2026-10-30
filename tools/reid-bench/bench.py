"""Can trucks be told apart by how they look? A bench on the yard's own frames.

Runs on the sample pictures fetched by relay/fetch-samples.mjs: one folder per
deck visit, with its frames (frame-NNN-HHMMSS.mmm.jpg), log.txt and visit.json.

For every frame, a small detector (YOLOv10n, the one the Electron app used)
finds the vehicles; the biggest is taken as the truck on the scale. Each crop
is turned into a fingerprint three ways - a colour histogram (no model),
DINOv2-small and CLIP ViT-B/32 - and the bench asks how well each separates:

  - same truck: two crops from one visit, a while apart;
  - different trucks: crops from two different visits. (A truck that comes
    back to be weighed empty counts as "different" here, so the scores are a
    little pessimistic.)

Visits with back-to-back trucks (the log says one pulled on before the one
ahead had left) are kept out of that, and checked on their own: the truck
before it began to move against what was on the deck at the end.

    python3 -m venv ~/reid-venv && ~/reid-venv/bin/pip install onnxruntime numpy pillow
    ~/reid-venv/bin/python tools/reid-bench/bench.py ~/yard-samples --out ~/reid-bench-out

Models are downloaded once into ~/.cache/reid-bench. Writes report.md and a
few contact sheets (JPEG) into --out.
"""

import argparse
import json
import random
import re
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image, ImageDraw

CACHE = Path.home() / ".cache" / "reid-bench"
MODELS = {
    "yolov10n": "https://huggingface.co/onnx-community/yolov10n/resolve/main/onnx/model.onnx",
    "dinov2-small": "https://huggingface.co/onnx-community/dinov2-small/resolve/main/onnx/model.onnx",
    "clip-b32": "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model.onnx",
}
# COCO: car, motorcycle, bus, train, truck. "train" because a truck head-on
# at night comes back as one (see src/main/services/vehicleDetector.js).
VEHICLES = {2, 3, 5, 6, 7}
MIN_SCORE = 0.3
FRAME_NAME = re.compile(r"frame-(\d+)-(\d{6})\.(\d{3})\.jpg$")
LOG_LINE = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3})Z (.*)$")
# lib.rs logs this when the scale weighs the next truck before the deck cleared.
FOLLOWS = "the next truck: it pulled on before"


def model(name):
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / f"{name}.onnx"
    if not path.exists():
        print(f"downloading {name} ...", file=sys.stderr)
        tmp = path.with_suffix(".part")
        urllib.request.urlretrieve(MODELS[name], tmp)
        tmp.rename(path)
    options = ort.SessionOptions()
    options.log_severity_level = 3
    return ort.InferenceSession(str(path), options, providers=["CPUExecutionProvider"])


# ── Visits ──────────────────────────────────────────────────────────────────


class Visit:
    def __init__(self, folder):
        self.folder = folder
        self.name = f"{folder.parent.name}/{folder.name}"
        self.info = json.loads((folder / "visit.json").read_text()) if (folder / "visit.json").exists() else {}
        self.log = (folder / "log.txt").read_text(errors="replace") if (folder / "log.txt").exists() else ""
        self.frames = sorted(folder.glob("frame-*.jpg"))
        self.times = self._frame_times()
        # The log reaches back 30 s before the visit, so only lines after
        # its start count.
        start = self._start()
        self.handover = next((t for t, text in self.events() if FOLLOWS in text and (start is None or t >= start)), None)
        self.follows = self.handover is not None

    def _frame_times(self):
        """Each frame's time in UTC. Frames are named in local time; the
        visit's start (UTC) gives the offset, rounded to 15 minutes."""
        started = self.info.get("started")
        stamps = []
        for f in self.frames:
            m = FRAME_NAME.search(f.name)
            stamps.append((m.group(2), m.group(3)) if m else None)
        if not started or not stamps or stamps[0] is None:
            return [None] * len(self.frames)
        start = datetime.fromisoformat(started.replace("Z", "+00:00"))
        day = start.date()
        first = datetime.strptime(stamps[0][0], "%H%M%S").replace(year=day.year, month=day.month, day=day.day, tzinfo=timezone.utc)
        offset = round((first - start).total_seconds() / 900) * 900
        out = []
        prev = None
        for s in stamps:
            if s is None:
                out.append(None)
                continue
            t = datetime.strptime(s[0], "%H%M%S").replace(year=day.year, month=day.month, day=day.day, microsecond=int(s[1]) * 1000, tzinfo=timezone.utc)
            t -= timedelta(seconds=offset)
            if prev and t < prev:  # past midnight
                t += timedelta(days=1)
            out.append(t)
            prev = t
        return out

    def _start(self):
        started = self.info.get("started")
        return datetime.fromisoformat(started.replace("Z", "+00:00")) if started else None

    def events(self):
        """(time, text) for the log lines about trucks moving and weighing."""
        out = []
        for line in self.log.splitlines():
            m = LOG_LINE.match(line)
            if m and re.search(r"moving|weighed|pulled on|next truck|deck clear|coming onto|same plate", m.group(2)):
                out.append((datetime.fromisoformat(m.group(1)).replace(tzinfo=timezone.utc), m.group(2)))
        return out


def load_visits(root):
    return [Visit(d) for d in sorted(root.glob("*/*")) if d.is_dir() and not d.name.endswith(".partial") and any(d.glob("frame-*.jpg"))]


# ── Detection ───────────────────────────────────────────────────────────────


def detect(session, image):
    """Vehicle boxes [x1, y1, x2, y2, score] in the image's pixels."""
    w, h = image.size
    scale = 640 / max(w, h)
    nw, nh = round(w * scale), round(h * scale)
    canvas = Image.new("RGB", (640, 640), (114, 114, 114))
    canvas.paste(image.resize((nw, nh), Image.BILINEAR), (0, 0))
    x = np.asarray(canvas, dtype=np.float32).transpose(2, 0, 1)[None] / 255.0
    out = session.run(None, {session.get_inputs()[0].name: x})[0][0]  # 300 x [x1 y1 x2 y2 score class]
    boxes = []
    for x1, y1, x2, y2, score, cls in out:
        if score >= MIN_SCORE and int(cls) in VEHICLES:
            boxes.append([x1 / scale, y1 / scale, x2 / scale, y2 / scale, float(score)])
    return boxes


def biggest(boxes):
    return max(boxes, key=lambda b: (b[2] - b[0]) * (b[3] - b[1]), default=None)


def crop(image, box, pad=0.05):
    x1, y1, x2, y2 = box[:4]
    pw, ph = (x2 - x1) * pad, (y2 - y1) * pad
    return image.crop((max(0, x1 - pw), max(0, y1 - ph), min(image.width, x2 + pw), min(image.height, y2 + ph)))


# ── Fingerprints ────────────────────────────────────────────────────────────


def unit(v):
    v = np.asarray(v, dtype=np.float32).ravel()
    return v / (np.linalg.norm(v) + 1e-9)


def histogram(image):
    hsv = np.asarray(image.convert("HSV").resize((128, 128)), dtype=np.int32)
    h, s, v = hsv[..., 0] * 8 // 256, hsv[..., 1] * 4 // 256, hsv[..., 2] * 4 // 256
    counts = np.bincount((h * 16 + s * 4 + v).ravel(), minlength=128).astype(np.float32)
    return unit(np.sqrt(counts))


class Embedder:
    def __init__(self, name, mean, std):
        self.session = model(name)
        self.mean = np.array(mean, dtype=np.float32)[:, None, None]
        self.std = np.array(std, dtype=np.float32)[:, None, None]
        self.outputs = [o.name for o in self.session.get_outputs()]

    def __call__(self, image):
        x = np.asarray(image.convert("RGB").resize((224, 224), Image.BICUBIC), dtype=np.float32).transpose(2, 0, 1) / 255.0
        x = ((x - self.mean) / self.std)[None]
        result = dict(zip(self.outputs, self.session.run(None, {self.session.get_inputs()[0].name: x})))
        for key in ("image_embeds", "pooler_output"):
            if key in result:
                return unit(result[key][0])
        return unit(result[self.outputs[0]][0][0])  # last_hidden_state, CLS token


# ── Scoring ─────────────────────────────────────────────────────────────────


def auc(pos, neg):
    """Chance a same-truck pair scores above a different-truck pair."""
    pos, neg = np.asarray(pos), np.asarray(neg)
    if not len(pos) or not len(neg):
        return float("nan")
    order = np.argsort(np.concatenate([pos, neg]))
    ranks = np.empty(len(order))
    ranks[order] = np.arange(1, len(order) + 1)
    return (ranks[: len(pos)].sum() - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg))


def at_false_alarms(pos, neg, rate):
    """The threshold letting through `rate` of different-truck pairs, and the
    share of same-truck pairs above it."""
    threshold = float(np.quantile(neg, 1 - rate))
    return threshold, float(np.mean(np.asarray(pos) > threshold))


def sheet(pairs, path, title, size=220):
    """A contact sheet of (crop, crop, caption) rows."""
    if not pairs:
        return
    rows = []
    for a, b, caption in pairs:
        row = Image.new("RGB", (size * 2 + 10, size + 24), "white")
        for i, im in enumerate((a, b)):
            im = im.copy()
            im.thumbnail((size, size))
            row.paste(im, (i * (size + 10), 0))
        ImageDraw.Draw(row).text((4, size + 4), caption, fill="black")
        rows.append(row)
    out = Image.new("RGB", (rows[0].width, sum(r.height for r in rows) + 24), "white")
    ImageDraw.Draw(out).text((4, 4), title, fill="black")
    y = 24
    for r in rows:
        out.paste(r, (0, y))
        y += r.height
    out.save(path, quality=85)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("samples", type=Path)
    p.add_argument("--out", type=Path, default=Path("reid-bench-out"))
    p.add_argument("--max-visits", type=int, default=0, help="for a quick try")
    p.add_argument("--every", type=int, default=3, help="look at every Nth frame")
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    random.seed(1)

    visits = load_visits(args.samples)
    if args.max_visits:
        visits = visits[: args.max_visits]
    print(f"{len(visits)} visits, {sum(len(v.frames) for v in visits)} frames", file=sys.stderr)

    detector = model("yolov10n")
    embedders = {
        "histogram": histogram,
        "dinov2-small": Embedder("dinov2-small", [0.485, 0.456, 0.406], [0.229, 0.224, 0.225]),
        "clip-b32": Embedder("clip-b32", [0.4815, 0.4578, 0.4082], [0.2686, 0.2613, 0.2758]),
    }
    timing = {k: 0.0 for k in ["detect", *embedders]}
    counts = {k: 0 for k in timing}

    # Per visit: the truck crops, with their frame index and fingerprints.
    crops = {}
    no_vehicle = 0
    for n, v in enumerate(visits, 1):
        items = []
        for i in range(0, len(v.frames), args.every):
            image = Image.open(v.frames[i]).convert("RGB")
            t = time.perf_counter()
            box = biggest(detect(detector, image))
            timing["detect"] += time.perf_counter() - t
            counts["detect"] += 1
            if box is None:
                no_vehicle += 1
                continue
            c = crop(image, box)
            prints = {}
            for name, embed in embedders.items():
                t = time.perf_counter()
                prints[name] = embed(c)
                timing[name] += time.perf_counter() - t
                counts[name] += 1
            items.append({"i": i, "time": v.times[i], "crop": c, "box": box, "prints": prints})
        crops[v.name] = items
        print(f"[{n}/{len(visits)}] {v.name}: {len(items)} crops", file=sys.stderr)

    plain = [v for v in visits if not v.follows and len(crops[v.name]) >= 2]
    b2b = [v for v in visits if v.follows]

    # Two crops a third and two thirds of the way through each plain visit.
    picks = {}
    for v in plain:
        items = crops[v.name]
        picks[v.name] = (items[len(items) // 3], items[(2 * len(items)) // 3])

    report = [
        "# Truck re-identification bench",
        "",
        f"{len(visits)} visits ({len(plain)} with one truck and two usable crops, {len(b2b)} back-to-back), "
        f"{sum(len(v.frames) for v in visits)} frames, every {args.every} looked at; "
        f"{no_vehicle} frames had no vehicle found.",
        "",
        "## Same truck vs different trucks",
        "",
        "AUC: the chance a same-truck pair scores above a different-truck pair (1.0 perfect, 0.5 a coin toss). "
        "'Caught at 1%' is the share of same-truck pairs recognised when the threshold lets through only 1% of different-truck pairs.",
        "",
        "| fingerprint | AUC | caught at 1% false alarms | caught at 5% | threshold at 1% | ms per crop |",
        "|---|---|---|---|---|---|",
    ]
    thresholds = {}
    hardest = {}
    names = list(picks)
    for name in embedders:
        pos = [float(a["prints"][name] @ b["prints"][name]) for a, b in picks.values()]
        neg, neg_pairs = [], []
        for x in range(len(names)):
            for y in range(x + 1, len(names)):
                a, b = picks[names[x]][0], picks[names[y]][1]
                s = float(a["prints"][name] @ b["prints"][name])
                neg.append(s)
                neg_pairs.append((s, names[x], names[y], a, b))
        if not pos or not neg:
            report.append(f"| {name} | not enough visits | | | | |")
            continue
        t1, r1 = at_false_alarms(pos, neg, 0.01)
        _, r5 = at_false_alarms(pos, neg, 0.05)
        thresholds[name] = t1
        ms = 1000 * timing[name] / max(1, counts[name])
        report.append(f"| {name} | {auc(pos, neg):.3f} | {r1:.0%} | {r5:.0%} | {t1:.3f} | {ms:.0f} |")
        hardest[name] = sorted(neg_pairs, key=lambda x: -x[0])[:8]
    report.append("")
    report.append(f"Detector: {1000 * timing['detect'] / max(1, counts['detect']):.0f} ms per frame.")

    # Back-to-back visits: the truck before it moved vs the deck at the end.
    report += ["", "## Back-to-back visits", ""]
    if not b2b:
        report.append("None in these samples.")
    else:
        report.append("Before: the truck's last crop before the log says it began to move. After: the last crop of the visit. "
                      "A score under the 1% threshold means the fingerprint says a different truck.")
        report.append("")
        report.append("| visit | " + " | ".join(f"{n} (under {thresholds.get(n, float('nan')):.3f}?)" for n in embedders) + " |")
        report.append("|---|" + "---|" * len(embedders))
        rows = []
        for v in b2b:
            items = crops[v.name]
            moving = max((t for t, text in v.events() if "moving" in text and t <= v.handover), default=None)
            same_plate = any("same plate" in text and t >= v.handover for t, text in v.events())
            before = [c for c in items if c["time"] and moving and c["time"] < moving]
            if not before or len(items) < 2:
                report.append(f"| {v.name} | not enough crops around the move | |")
                continue
            a, b = before[-1], items[-1]
            cells = []
            for n in embedders:
                s = float(a["prints"][n] @ b["prints"][n])
                cells.append(f"{s:.3f} {'different' if s < thresholds.get(n, -1) else 'same'}")
            note = " (app: same plate, so same truck)" if same_plate else ""
            report.append(f"| {v.name}{note} | " + " | ".join(cells) + " |")
            rows.append((a["crop"], b["crop"], f"{v.name}{note}  before move / end"))
        sheet(rows, args.out / "back-to-back.jpg", "Back-to-back visits: before the move | at the end")

    for name, pairs in hardest.items():
        sheet([(a["crop"], b["crop"], f"{s:.3f}  {x}  vs  {y}") for s, x, y, a, b in pairs], args.out / f"hardest-different-{name}.jpg",
              f"{name}: the most alike pairs from different visits")
    sample = random.sample(list(picks.items()), min(8, len(picks)))
    sheet([(a["crop"], b["crop"], k) for k, (a, b) in sample], args.out / "same-truck-examples.jpg", "Same visit, a third and two thirds through")

    report += ["", "## Files", "", "- back-to-back.jpg", "- hardest-different-<fingerprint>.jpg: different trucks each method found most alike",
               "- same-truck-examples.jpg"]
    (args.out / "report.md").write_text("\n".join(report) + "\n")
    print("\n".join(report))


if __name__ == "__main__":
    main()
