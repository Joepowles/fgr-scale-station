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
| camera stream / snapshots      | —                          | not yet (ffmpeg sidecar) |
| ONVIF discovery                | —                          | not yet |
| vehicle detector               | —                          | not yet (`ort` crate) |
| plate reader                   | —                          | not yet |
| history, weighing pipeline     | —                          | not yet |
| bug report, updater            | —                          | not yet |

Calls for the parts not yet ported answer "not ported to Rust yet".

One deliberate difference: the Electron `FrameSplitter` drops the last
character of a frame that ends at an STX with no line ending after it, which
loses the motion flag on indicators that send no CR/LF. The Rust one keeps it
(`keeps_the_status_letter_of_frames_split_on_stx`).

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

### On a headless Linux box

GTK blocks for 25 s per D-Bus call when there is no session bus, so run the
window under one, and a virtual display:

    dbus-run-session -- xvfb-run -a npm run tauri dev

## Building for Windows

Build on Windows (Rust via rustup, plus the WebView2 runtime that Windows 11
already has):

    npm install
    npm run tauri build        # NSIS installer under src-tauri/target/release/bundle
