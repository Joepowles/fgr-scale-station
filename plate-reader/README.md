# plate-reader

The station's licence plate reader: fast-alpr's two open models (a YOLO v9
plate detector and a CCT character reader) on ONNX Runtime, with fast-alpr's
preprocessing and decoding reproduced in Rust. It replaced a bundled Python
with fast-alpr - about 270 MB of the installer - and is driven by
`src/main/services/plateReader.js` over the same JSON-lines protocol the
Python worker spoke (see `src/main.rs`).

    node scripts/build-plate-reader.js     # builds it into resources/bin, models into resources/plate-models
    cargo test                             # in this folder

On Windows the exe has ONNX Runtime linked in. Elsewhere (development) the
prebuilt static runtime needs a newer glibc than most boxes have, so it is
loaded at start-up from `ORT_DYLIB_PATH` instead.

## Checked against fast-alpr

`compare/` has the Python worker it replaced and a script that runs both on a
folder of pictures. On 28 pictures - plates at many sizes, pasted into 4K and
substream frames from the yard camera, re-compressed - both gave the same
text for every one, boxes within 1 px, confidences within 0.05:

    python3 compare/compare.py PICTURES ~/venv/bin/python compare/plate_reader_worker.py target/release/plate-reader --models DIR

Run it again after changing the preprocessing, or with pictures of real
trucks.
