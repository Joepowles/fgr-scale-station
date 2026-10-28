# Falcon Scale Station

A standalone Windows 11 app for a yard with a truck scale and a camera over
it. One window, almost all camera, with the live weight and the plate read off
each truck as overlays. Every weighing is photographed and written to a local
history with a retention period you choose. Nothing depends on the internet.

Built from the same pieces as the scale camera card on falconworkorders.com:
the scale frame parser, the vehicle detector, the plate reader and the camera
streaming code are the Falcon backend's, copied in.

## Two builds of one app

From 0.2.0 the app is built in Rust on Tauri, in `rust/` (see its README);
the Electron app here is 0.1.x, kept until every yard has moved over. Both
show the same window (`src/renderer`), read the same release feed and write
the same history, and the Rust app's installer takes the Electron app off a
PC and carries on from its settings. Tags `v0.1.*` build the Electron app,
newer ones the Rust app.

## What it does

- **Camera** - finds cameras on the LAN over ONVIF, lists their streams, shows
  the one you pick full-window, and grabs frames from the full-resolution
  stream for plate reads and history photos.
- **Plates** - reads plates locally with fast-alpr's open models, run by a
  small Rust helper (plate-reader/) with the models bundled; no cloud.
- **Scale** - finds the Moxa NPort on the LAN, reads the indicator's serial
  stream over TCP, shows the live weight, and fires when a weight settles.
- **History** - a JSON-lines log plus photos in a folder you choose, kept for
  as many days as you choose (a week by default).

## Developing

    npm install
    npm run dev          # Electron with hot reload (needs ffmpeg on the PATH)
    npm test

## Building the installer

Done by `.github/workflows/release.yml` on a Windows runner when a `v0.1.*` tag is
pushed (newer tags build `rust/`): it fetches ffmpeg, builds the plate reader and fetches its models, packages with electron-builder and publishes the installer to the
GitHub release. The installed app auto-updates from those releases.

Merging a pull request into main releases it
(`.github/workflows/release-after-merge.yml`): the next patch version, the
PR's entry at the top of `CHANGELOG.md`, a tag, and the build above. The
entry is the PR's `## Changelog` section, written for the yard, or its title
if it has none. Label a PR `no-release` to merge it without a release.

To build by hand on a Windows machine:

    node scripts/make-icon.js
    node scripts/fetch-ffmpeg.js
    node scripts/build-plate-reader.js     # needs Rust: https://rustup.rs
    npm run dist:win
