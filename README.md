# Falcon Scale Station

A standalone Windows 11 app for a yard with a truck scale and a camera over
it. One window, almost all camera, with the live weight and the plate read off
each truck as overlays. Every weighing is photographed and written to a local
history with a retention period you choose. Nothing depends on the internet.

Built from the same pieces as the scale camera card on falconworkorders.com:
the scale frame parser, the vehicle detector, the plate reader and the camera
streaming code are the Falcon backend's, copied in.

## What it does

- **Camera** - finds cameras on the LAN over ONVIF, lists their streams, shows
  the one you pick full-window, and grabs frames from the full-resolution
  stream for plate reads and history photos.
- **Plates** - reads plates locally with fast-alpr (bundled Python, bundled
  models), no cloud.
- **Scale** - finds the Moxa NPort on the LAN, reads the indicator's serial
  stream over TCP, shows the live weight, and fires when a weight settles.
- **History** - a JSON-lines log plus photos in a folder you choose, kept for
  as many days as you choose (a week by default).

## Developing

    npm install
    npm run dev          # Electron with hot reload (needs ffmpeg on the PATH)
    npm test

## Building the installer

Done by `.github/workflows/release.yml` on a Windows runner when a `v*` tag is
pushed: it fetches ffmpeg, builds the embedded Python with fast-alpr and its
models, packages with electron-builder and publishes the installer to the
GitHub release. The installed app auto-updates from those releases.

To build by hand on a Windows machine:

    node scripts/make-icon.js
    node scripts/fetch-ffmpeg.js
    node scripts/build-python.js
    npm run dist:win
