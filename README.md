<div align="center">

# Falcon Scale Station

**Truck scale and camera monitoring for the weighbridge PC.**

Live weight, automatic licence plate reads and a photographed history of every
weighing, in a single window that runs entirely on the local network.

[![Latest release](https://img.shields.io/github/v/release/Joepowles/fgr-scale-station?label=release)](https://github.com/Joepowles/fgr-scale-station/releases/latest)
[![Release build](https://img.shields.io/github/actions/workflow/status/Joepowles/fgr-scale-station/release.yml?label=build)](https://github.com/Joepowles/fgr-scale-station/actions/workflows/release.yml)
![Platform](https://img.shields.io/badge/platform-Windows%2011-0078D4)
![Built with](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri%202-B7410E)

</div>

---

## Overview

Falcon Scale Station is a standalone Windows desktop application for sites with
a truck scale and an IP camera over the deck. The window is almost entirely
camera, with the live weight and the plate of the truck on the scale shown as
overlays. When a truck settles, the station records its weight, reads its
plate, takes a full-resolution photo, stamps it and writes it to a local
history.

Everything happens on the site's own network. Plate recognition runs on the
PC, no account or cloud service is needed, and the app keeps working when the
internet does not.

## Features

### Scale
- Finds the serial device server (Moxa NPort) on the LAN and reads the
  indicator's stream over TCP.
- Detects when a truck comes on, settles and leaves, with configurable
  minimum, clear and stability thresholds.
- Weighs a truck that never fully settles once its reading holds within a set
  band, and marks the entry accordingly.
- **Back-to-back trucks.** Holds a weighed truck at its weight until it moves.
  When the next truck pulls on before the deck reads clear, it gets an entry
  of its own, and the plate read as it came on, instead of being folded into
  the truck ahead. The log shows each state change and the readings around it.
- **Shared connection.** When the app is open on several PCs, one reads the
  scale and relays it to the others, so the device server only ever sees one
  connection. Leadership moves automatically if that PC closes or goes down.

### Camera
- Discovers cameras over ONVIF (WS-Discovery plus a LAN sweep) and lists
  their streams.
- Plays the low-resolution stream live and switches to full resolution when
  zoomed in. Scroll to zoom about the pointer, drag a box to zoom into it,
  right-click to reset.
- Takes photos from the full-resolution stream, decoding keyframes only so a
  photo is never a half-drawn frame.

### Plate recognition
- Reads plates on the PC with the open fast-alpr detection and OCR models on
  ONNX Runtime, bundled in the installer.
- Reads from the moment a truck rolls on, about once a second, and keeps the
  surest plate across every picture.
- Picks the truck's plate from everything in frame: the largest or clearest
  read, optionally favouring one side of the picture. Ignore lists and a
  minimum confidence filter out signage and misreads.

### History
- A JSON-lines log plus stamped photos in a folder of your choice, kept for
  as many days as you choose (seven by default).
- A history panel beside the picture, or popped out into its own window on
  another screen.
- **Gross, tare and net.** Two weighings of the same plate less than an hour
  apart are paired: the heavier is the gross, the lighter the tare, and the
  history and the live picture show the net. A truck making load after load
  in the hour pairs each load with its own empty weighing, and a load whose
  empty weighing was missed (or a truck tared once) uses the nearest tare.
  A truck's appearance is meant to
  pair trucks with no plate the same way once it can be recognised.

### Alerts
- Rules made of a *when* and a *then*: something on the scale, a truck
  weighed (optionally over or under a weight), a plate read or on a list, no
  plate read, an unsettled truck, motion in the picture, or the scale or
  camera going quiet.
- Sounds (built-in or your own file), on-screen banners and Windows
  notifications, with quiet hours, a master volume, snooze from the tray and
  a cooldown per alert.

### Reliability and support
- A watchdog checks that the window, the scale reader and the pipeline behind
  them are answering, reports a crash or freeze with its log and restarts the
  app.
- A Bug report tab files problems and ideas straight to this repository's
  issues, with the station's settings (secrets redacted), recent weighings
  and log tail attached.
- Updates itself from GitHub Releases on a Stable or Beta channel, resuming
  interrupted downloads.
- Starts with Windows if wanted, runs as a single instance and remembers its
  window position.

## Installation

1. Download the latest `Falcon Scale Station` installer from the
   [Releases page](https://github.com/Joepowles/fgr-scale-station/releases/latest).
2. Run it. The installer bundles ffmpeg, the plate reader and its models;
   Windows 11 already has the WebView2 runtime the app needs.
3. Open **Settings** and set up the **Camera** and **Scale** tabs. Both can
   search the network for their device.
4. If Windows asks whether to let the app use the network, allow it on
   private networks so PCs can share the scale connection.

Installed copies check for updates on their own, download and verify them in
the background and install on the next restart. Release notes are on the
**Updates** tab in Settings and in
[CHANGELOG.md](CHANGELOG.md).

## Architecture

| Layer | Technology | Location |
|---|---|---|
| Application core | Rust on [Tauri 2](https://v2.tauri.app), Tokio, Axum | `rust/src-tauri/` |
| User interface | React 19, Tailwind CSS 4, Vite | `src/renderer/`, `rust/src/` |
| Plate reader | Rust, ONNX Runtime, fast-alpr models | `plate-reader/` |
| Video | ffmpeg, bundled as a Tauri sidecar | fetched at build time |
| Bug report relay | Cloudflare Worker | `relay/` |
| Installer | NSIS via the Tauri bundler | `rust/src-tauri/windows/` |

The Rust core owns every device connection. A loopback-only HTTP server
(`server.rs`) gives the window its live stream, snapshots, history photos and
alert sounds; everything else goes over Tauri's `invoke` and events.

```
rust/src-tauri/src/
├── scale/          frame parsing, reader, NPort discovery, connection sharing
├── camera/         ONVIF, discovery, RTSP, ffmpeg streams, snapshots, motion
├── pipeline.rs     on, settled, off: weighing, plate bursts and photos
├── plates.rs       drives the plate-reader helper
├── plate_choice.rs picks the truck's plate from everything in frame
├── history.rs      log, photos and retention
├── alerts.rs       alert rules, quiet hours and the tray
├── updater.rs      release feed and resumable downloads
├── crash.rs        watchdog and automatic crash reports
└── bug_report.rs   Bug report tab
```

## Development

### Prerequisites

- [Rust](https://rustup.rs) 1.88 or newer
- Node.js 20 or newer
- ffmpeg (Linux: the system package; Windows: fetched by a script)

### Run the app

```sh
cd rust
npm install
npm run fetch:ffmpeg            # copy ffmpeg into the sidecar folder
npm run build:plate-reader      # build the plate reader and fetch its models
npm run tauri dev               # hot-reloading window; Rust rebuilds on save
```

On Linux the plate reader loads ONNX Runtime from `ORT_DYLIB_PATH`, and the
window's video needs `gstreamer1.0-libav`. On a headless box, run under
`dbus-run-session -- xvfb-run -a npm run tauri dev`.

### Test

```sh
cd rust/src-tauri
cargo test
```

Tests that need real hardware or the network are `#[ignore]`d and run with
`cargo test <name> -- --ignored`.

### No yard required

`rust/tools/` has stand-ins for every device, so the whole app can be run and
tested away from site:

| Tool | Simulates |
|---|---|
| `node tools/fake-indicator.cjs 9950` | A scale indicator: a truck rolls on, settles at 48,320 lb and leaves, on a loop |
| `python3 tools/fake-camera.py` | An HEVC RTSP camera with sub and main streams (login `admin` / `secret`) |
| `python3 tools/fake-onvif.py --port 8899` | ONVIF discovery and device calls, pointing at the fake camera |

See [rust/README.md](rust/README.md) for the full developer notes.

## Releases

Releases are fully automated. **Merging a pull request into `main` ships it.**

1. The **Release after merge** workflow bumps the patch version in
   `package.json`, `tauri.conf.json` and `Cargo.toml`, adds the PR's entry to
   the top of `CHANGELOG.md`, commits and tags `vX.Y.Z`.
2. The **Release** workflow builds and tests on a Windows runner, packages
   the NSIS installer with ffmpeg and the plate reader, and publishes it with
   its update feed to GitHub Releases.
3. Installed stations pick up the update automatically.

**Writing the changelog entry.** The app shows the changelog to the people
using it, so write it for them. Add a `## Changelog` section to the PR
description; each `-` bullet becomes one entry. Without one, the PR title is
used.

**What is not released:**
- PRs labelled `no-release`.
- PRs that touch nothing the app is built from, such as docs, workflows or
  the relay.
- PRs that set the version themselves (changing `tauri.conf.json`); tag those
  by hand.

A merged PR that was skipped or whose run failed can be released from the
Actions tab: run **Release after merge** with the PR number. Pre-release tags
such as `v0.3.0-beta.1` publish to the Beta channel only.

## Bug reports

Reports sent from the app's **Bug report** tab, and automatic crash and freeze
reports, arrive as issues labelled
[`from-the-yard`](https://github.com/Joepowles/fgr-scale-station/issues?q=label%3Afrom-the-yard),
plus `bug` for problems or `enhancement` for ideas. They go through a small
Cloudflare Worker (`relay/`) that holds the GitHub token, so no credential
ships in the installer.

## Repository layout

```
rust/              the application (Tauri core, window entry, dev tools)
src/renderer/      the React user interface
plate-reader/      the licence plate recognition helper
relay/             Cloudflare Worker for bug reports
scripts/           build and release scripts
.github/workflows/ release automation
CHANGELOG.md       release notes, shown in the app
```
