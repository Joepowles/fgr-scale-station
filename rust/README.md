# Falcon Scale Station, in Rust

The station app with its main process in Rust on [Tauri 2](https://v2.tauri.app),
replacing the Electron app in the rest of this repository from 0.2.0. The
window is the Electron app's renderer, built from `../src/renderer` (see
`src/main.jsx`), so both apps show the same window; `src/station.js` rebuilds
the `window.station` API the Electron preload provides, on Tauri's `invoke()`
and `listen()`.

## What came from where

| Electron (`src/main/…`)        | Rust (`src-tauri/src/…`)   | Notes |
|--------------------------------|----------------------------|-------|
| `services/scaleFrames.js`      | `scale/frames.rs`          | tests ported |
| `services/scaleReader.js`      | `scale/reader.rs`          | tests ported |
| `settings.js`                  | `settings.rs`              | |
| `index.js` (log, IPC, window)  | `lib.rs`                   | |
| `windowState.js`               | `window_state.rs`          | same window-state.json |
| `services/nportDiscovery.js`   | `scale/discovery.rs`       | tests ported and extended |
| `services/cameraStream.js`     | `camera/stream.rs`         | ffmpeg as a Tauri sidecar |
| `services/rtspSnapshot.js`     | `camera/snapshot.rs`, `camera/rtsp.rs` | |
| `services/cameraDiscovery.js`  | `camera/discovery.rs`, `camera/onvif.rs` | |
| `services/history.js`          | `history.rs`               | same files; tests ported and extended |
| local HTTP server              | `server.rs`                | /stream, /snapshot, /photo |
| `services/weighingPipeline.js` | `pipeline.rs`              | no vehicle detector, see below |
| `services/plateReader.js`      | `plates.rs`                | the same plate-reader helper |
| `services/plateFilter.js`      | `plate_choice.rs`          | and which plate in the frame |
| photo stamping (`annotate`)    | `stamp.rs`                 | font compiled in |
| `services/bugReport.js`        | `bug_report.rs`            | tests ported |
| electron-updater, `updateChannel.js` | `updater.rs`         | same feed, resumable downloads |
| vehicle detector               | —                          | dropped: the scale is the trigger |
| —                              | `electron_import.rs`, `windows/installer-hooks.nsh` | taking over from the Electron app |

Deliberate differences from the Electron app:

- No vehicle detector: the scale says when a truck is there, and the reader
  finds every plate in the whole full-resolution frame rather than in a crop
  around a detected vehicle. `plate_choice.rs` then takes the truck's: the
  largest plate read well enough, favouring one side of the picture (the
  right, by default, by up to 50%) - set on the Plates tab. The Electron app
  took the biggest vehicle, which a truck parked behind the scale could be
  when the truck on the deck was not recognised as a vehicle.
- Updates resume a dropped download instead of starting it again, and never
  step down to an Electron build (below 0.2.0): that would install beside
  this app rather than over it.
- ONVIF replies are read with hyphens and dots allowed in namespace
  prefixes, so a gSOAP camera's `SOAP-ENV:` fault reason ("Sender not
  authorized") is shown rather than a bare "SOAP fault".
- Snapshots decode keyframes only (`-skip_frame nokey`), so a photo taken
  by joining the stream between keyframes is never a grey smear. It can
  wait up to one keyframe interval longer for the picture.
- Its `FrameSplitter` drops the last character of a frame that ends at an
  STX with no line ending after it, which loses the motion flag on
  indicators that send no CR/LF. The Rust one keeps it
  (`keeps_the_status_letter_of_frames_split_on_stx`).
- Its history prune removes every photo no log line names; with "keep the
  log" off that is every photo, within the hour. The Rust one removes
  unnamed photos only while the log is kept, and otherwise by age
  (`can_keep_photos_without_the_log_and_they_survive_a_prune`).

## Developing

    npm install
    npm run tauri dev          # window with hot reload; Rust rebuilds on save
    cd src-tauri && cargo test # the Rust tests

Settings and `station.log` live in the app data folder for the identifier
`com.falcongreenresources.scalestation.rs` (separate from the Electron app's),
e.g. `~/.local/share/com.falcongreenresources.scalestation.rs` on Linux. On
its first start with none there, the app takes the Electron app's settings,
window place and log, and keeps using its history folder
(`electron_import.rs`).

The updater only runs in a release build; `STATION_UPDATER=1` turns it on in
development. `cargo test real_feed -- --ignored` reads the live release feed.

### A scale to read without a yard

    node tools/fake-indicator.cjs 9950

then in Settings → Scale, address `127.0.0.1`, port `9950`. It plays a truck
rolling on, settling at 48,320 lb, and leaving, on a loop.

### A camera to watch without a yard

    python3 tools/fake-camera.py

serves HEVC over RTSP, as the yard's cameras do, with the login
admin / secret: `rtsp://127.0.0.1:8554/sub` (960x480) for the live view and
`/main` (1920x1080) for photos. It needs GStreamer's RTSP server (Debian:
`gir1.2-gst-rtsp-server-1.0 python3-gi gstreamer1.0-plugins-{good,bad,ugly}`).
The tests that use it are ignored by default:

    cargo test fake_camera -- --ignored --nocapture

### The plate reader

The app drives `plate-reader` (`../plate-reader`, the Electron app's helper)
with its models, from `resources/bin` and `resources/plate-models` at the
repository root, where `node ../scripts/build-plate-reader.js` puts them. On
Linux the helper loads ONNX Runtime from `ORT_DYLIB_PATH`; the one
onnxruntime-node ships in the Electron app's `node_modules` does:

    export ORT_DYLIB_PATH=$PWD/../node_modules/onnxruntime-node/bin/napi-v6/linux/x64/libonnxruntime.so.1
    PLATE_READER_EXE=$PWD/../resources/bin/plate-reader PLATE_READER_MODELS=$PWD/../resources/plate-models npm run tauri dev

and `python3 tools/fake-camera.py --image photo-with-a-plate.jpg` gives it
something to read. The test that drives the real helper:

    PLATE_SAMPLE=photo-with-a-plate.jpg cargo test plate_reader -- --ignored --nocapture

### A camera to find without a yard

    python3 tools/fake-onvif.py --port 8899 [--no-discovery]

answers WS-Discovery and the ONVIF calls the Camera tab makes (login
admin / secret), pointing at `tools/fake-camera.py` for the streams. With
`--no-discovery` only the LAN sweep can find it. The search on request:

    cargo test finds_cameras_on_this_lan -- --ignored --nocapture

### ffmpeg

The app runs ffmpeg as a Tauri sidecar (`bundle.externalBin`), which has to
be in `src-tauri/binaries/` before a build:

    npm run fetch:ffmpeg     # Linux: copies the system ffmpeg in

On Linux the window's <video> also needs `gstreamer1.0-libav` for H.264.

### On a headless Linux box

GTK blocks for 25 s per D-Bus call when there is no session bus, so run the
window under one, and a virtual display:

    dbus-run-session -- xvfb-run -a npm run tauri dev

## Building for Windows

The release workflow (`../.github/workflows/release.yml`, the rust job) does
this for every tag from v0.2.0. By hand, on Windows (Rust via rustup, plus the
WebView2 runtime that Windows 11 already has):

    npm install
    node scripts/fetch-ffmpeg.cjs              # BtbN's static build
    node ../scripts/build-plate-reader.js      # the plate reader and its models
    npx tauri build                            # NSIS installer under src-tauri/target/release/bundle
    node scripts/write-update-feed.cjs         # renamed, with latest.yml / beta.yml, in dist-release/

The version is in three places, which the workflow checks against the tag:
`package.json`, `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml`.

The installer (`windows/installer-hooks.nsh`) takes the Electron app off the
PC first, keeping its data, and when the Electron app's updater started it
(`--force-run`) opens the new app afterwards.
