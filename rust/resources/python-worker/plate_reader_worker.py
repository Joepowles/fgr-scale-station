#!/usr/bin/env python3
"""Licence plate reader worker, driven by backend/services/plateReader.js.

Reads one JSON request per line on stdin and writes one JSON reply per line on
stdout. Keeping the process alive between requests is what makes a read cost
a fraction of a second rather than the two or three seconds the models take to
load.

Request:  {"id": 1, "path": "/tmp/crop.jpg"}
Reply:    {"id": 1, "plates": [{"text": "P865012", "confidence": 0.98,
           "charConfidence": [..], "detConfidence": 0.74, "box": [x1, y1, x2, y2]}]}
          or {"id": 1, "error": "..."}

Boxes are in pixels of the image given. The models are the open ones packaged
by the fast-alpr project (a YOLO plate detector and a CCT character reader),
fetched into the user's cache directory the first time they are used.
"""
import json
import sys
import time


def log(message):
    sys.stderr.write(f"[plate-reader] {message}\n")
    sys.stderr.flush()


def reply(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def main():
    try:
        started = time.time()
        from fast_alpr import ALPR
        alpr = ALPR(
            detector_model="yolo-v9-t-384-license-plate-end2end",
            ocr_model="cct-xs-v1-global-model",
        )
        log(f"models ready in {time.time() - started:.1f}s")
    except Exception as err:  # noqa: BLE001 - whatever it was, Node needs to hear it
        reply({"ready": False, "error": f"could not load the plate models: {err}"})
        return 1

    reply({"ready": True})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except json.JSONDecodeError as err:
            reply({"error": f"bad request: {err}"})
            continue
        request_id = request.get("id")
        path = request.get("path")
        try:
            started = time.time()
            results = alpr.predict(path)
            plates = []
            for result in results:
                ocr = result.ocr
                det = result.detection
                if ocr is None or not ocr.text:
                    continue
                chars = ocr.confidence if isinstance(ocr.confidence, (list, tuple)) else [ocr.confidence]
                chars = [float(c) for c in chars]
                box = det.bounding_box
                plates.append({
                    "text": str(ocr.text),
                    # The weakest character is the honest confidence for the
                    # whole read: one wrong digit is a wrong plate.
                    "confidence": min(chars) if chars else 0.0,
                    "charConfidence": chars,
                    "detConfidence": float(det.confidence),
                    "box": [int(box.x1), int(box.y1), int(box.x2), int(box.y2)],
                })
            reply({"id": request_id, "plates": plates, "ms": int((time.time() - started) * 1000)})
        except Exception as err:  # noqa: BLE001
            reply({"id": request_id, "error": str(err)})
    return 0


if __name__ == "__main__":
    sys.exit(main())
