//! The plate reader worker, driven by src/main/services/plateReader.js. It
//! replaces the Python worker (now compare/plate_reader_worker.py) and speaks the
//! same protocol, one JSON object per line:
//!
//!     plate-reader [--models DIR] [--threads N]
//!
//! First line out: {"ready": true}, or {"ready": false, "error": "..."}.
//! Then for each request {"id": 1, "path": "/tmp/crop.jpg"} on stdin:
//!     {"id": 1, "plates": [{"text": "P865012", "confidence": 0.98,
//!      "charConfidence": [..], "detConfidence": 0.74, "box": [x1, y1, x2, y2]}],
//!      "ms": 120}
//! or {"id": 1, "error": "..."}. Exits when stdin closes.
//!
//! DIR holds the two .onnx files; it defaults to "models" beside the exe.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use plate_reader::Reader;
use serde_json::{json, Value};

fn reply(out: &mut impl Write, payload: Value) {
    // Nothing useful to do if the app has gone; the read loop ends anyway.
    let _ = writeln!(out, "{payload}").and_then(|_| out.flush());
}

fn main() {
    let mut models = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("models"))).unwrap_or_else(|| PathBuf::from("models"));
    let mut threads = plate_reader::default_threads();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--models", Some(dir)) => models = PathBuf::from(dir),
            ("--threads", Some(n)) => threads = n.parse().unwrap_or(threads),
            _ => {
                eprintln!("usage: plate-reader [--models DIR] [--threads N]");
                std::process::exit(2);
            }
        }
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let started = Instant::now();
    let mut reader = match Reader::load(&models, threads) {
        Ok(reader) => reader,
        Err(err) => {
            reply(&mut out, json!({ "ready": false, "error": format!("could not load the plate models: {err}") }));
            std::process::exit(1);
        }
    };
    eprintln!("[plate-reader] models ready in {:.1}s ({threads} threads)", started.elapsed().as_secs_f32());
    reply(&mut out, json!({ "ready": true }));

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(err) => {
                reply(&mut out, json!({ "error": format!("bad request: {err}") }));
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let started = Instant::now();
        let result = match request.get("path").and_then(Value::as_str) {
            Some(path) => plate_reader::open(path.as_ref()).and_then(|image| reader.read(&image)),
            None => Err("no path in the request".to_string()),
        };
        match result {
            Ok(plates) => reply(&mut out, json!({ "id": id, "plates": plates, "ms": started.elapsed().as_millis() as u64 })),
            Err(err) => reply(&mut out, json!({ "id": id, "error": err })),
        }
    }
}
