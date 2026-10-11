"""Run the Python worker and the Rust worker on the same images through the
JSON-lines protocol and compare what they say.

    python3 compare.py IMAGE_DIR PYTHON plate_reader_worker.py PLATE_READER [--models DIR]

PYTHON is one with fast-alpr installed (pip install fast-alpr onnxruntime);
its first run downloads the models to ~/.cache. Off Windows the Rust worker
needs ORT_DYLIB_PATH: the libonnxruntime.so inside that same venv makes the
two run on the same runtime, so any difference is the Rust preprocessing.
"""
import json, os, subprocess, sys, time

def run(cmd, images, env=None):
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)
    t = time.time(); ready = json.loads(p.stdout.readline()); load = time.time() - t
    assert ready.get("ready"), ready
    out = {}
    for i, img in enumerate(images):
        p.stdin.write(json.dumps({"id": i, "path": img}) + "\n"); p.stdin.flush()
        out[img] = json.loads(p.stdout.readline())
    p.stdin.close(); p.wait()
    return load, out

d = sys.argv[1]
images = sorted(os.path.join(d, f) for f in os.listdir(d))
py_load, py = run(sys.argv[2:4], images, dict(os.environ, HOME=os.path.expanduser("~")))
rs_load, rs = run(sys.argv[4:], images)
print(f"load: python {py_load:.1f}s  rust {rs_load:.1f}s")
same_text = 0; worst_conf = worst_det = 0.0; worst_box = 0; total = 0; py_ms = rs_ms = 0
for img in images:
    a, b = py[img], rs[img]
    py_ms += a.get("ms", 0); rs_ms += b.get("ms", 0)
    ta = [x["text"] for x in a.get("plates", [])]; tb = [x["text"] for x in b.get("plates", [])]
    flag = "" if ta == tb else "   <-- DIFFERENT"
    print(f"{os.path.basename(img):26} py {ta!s:24} {a.get('ms')}ms | rust {tb!s:24} {b.get('ms')}ms{flag}")
    for x, y in zip(a.get("plates", []), b.get("plates", [])):
        total += 1; same_text += x["text"] == y["text"]
        worst_conf = max(worst_conf, max(abs(p - q) for p, q in zip(x["charConfidence"], y["charConfidence"])))
        worst_det = max(worst_det, abs(x["detConfidence"] - y["detConfidence"]))
        worst_box = max(worst_box, max(abs(p - q) for p, q in zip(x["box"], y["box"])))
        if x["text"] != y["text"] or abs(x["confidence"] - y["confidence"]) > 0.02:
            print(f"    py {x['text']} {x['confidence']:.3f} {x['box']}   rust {y['text']} {y['confidence']:.3f} {y['box']}")
print(f"plates compared {total}, same text {same_text}; largest difference: char confidence {worst_conf:.4f}, detector confidence {worst_det:.4f}, box {worst_box}px")
print(f"total read time: python {py_ms}ms  rust {rs_ms}ms")
