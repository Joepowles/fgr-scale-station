# Falcon Scale Station, in Rust

An experiment: the Electron app in `../fgr-scale-station` with its main
process rewritten in Rust on [Tauri 2](https://v2.tauri.app). The window is
the same React app, copied unchanged; `src/station.js` rebuilds the
`window.station` API the Electron preload used to provide, on Tauri's
`invoke()` and `listen()`.

## What is ported

| Electron (`src/main/…`)        | Rust (`src-tauri/src/…`)   | State |
|--------------------------------|----------------------------|-------|
| `services/scaleFrames.js`      | `scale/frames.rs`          | done, tests ported |
| `services/scaleReader.js`      | `scale/reader.rs`          | done, tests ported |
| `settings.js`                  | `settings.rs`              | done |
| `index.js` (scale, log, IPC)   | `lib.rs`                   | scale parts only |
| `services/nportDiscovery.js`   | `scale/discovery.rs`       | done, tests ported and extended |
| `services/cameraStream.js`     | `camera/stream.rs`         | done; ffmpeg as a Tauri sidecar |
| `services/rtspSnapshot.js`     | `camera/snapshot.rs`, `camera/rtsp.rs` | done |
| ONVIF discovery                | —                          | not yet (stream URLs can be typed in) |
| vehicle detector               | —                          | dropped: the scale is the trigger |
| plate reader                   | —                          | not yet |
| `services/history.js`          | `history.rs`               | done, tests ported and extended |
| local HTTP server              | `server.rs`                | done: /stream, /snapshot, /photo |
| `services/weighingPipeline.js` | `pipeline.rs`              | logs weighings; photo and plate next |
| bug report, updater            | —                          | not yet |

Calls for the parts not yet ported answer "not ported to Rust yet".

Deliberate differences from the Electron app:

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
e.g. `~/.local/share/com.falcongreenresources.scalestation.rs` on Linux.

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

Build on Windows (Rust via rustup, plus the WebView2 runtime that Windows 11
already has):

    npm install
    node scripts/fetch-ffmpeg.cjs x86_64-pc-windows-msvc   # BtbN's static build
    npm run tauri build        # NSIS installer under src-tauri/target/release/bundle
