# Changelog

Newest first. The app shows this file on the Changelog tab of its settings.

## 0.1.6

- "Find cameras" now searches every network this PC is on, not just the one the multicast reply came back on. Each network gets the ONVIF multicast probe and then an address-by-address sweep (the probe sent to each address, plus a knock on the usual ONVIF ports), so a camera is found even when multicast is blocked or the camera has discovery turned off. The list says which networks were searched, and the button shows progress.
- A camera picked from the list is asked for its streams at the port it answered on, and the ONVIF port setting is now used when listing streams for a typed-in address.
- An update download that fails on a poor connection is tried again after 2 minutes, then 5, 15, 30 and 60, instead of waiting for the next six-hourly check. The Changelog tab says when the next try is. Downloads are differential: only the parts of the installer that changed since the installed version are fetched (about 17 MB of 332 MB between 0.1.4 and 0.1.5).

## 0.1.5

- A "Check for updates" button at the top of the Changelog tab, with the result shown beside it.

## 0.1.4

- A Changelog tab in Settings, showing this list.
- The plate now defaults to the top left corner of the picture and the weight to the bottom left. An install still on the old corners is moved to the new ones once.

## 0.1.3

- A downloaded update shows a "Restart to update" button in the header instead of a hint. Clicking it closes the app, installs, and reopens it.

## 0.1.2

- Republished so each version has exactly one release on GitHub; the 0.1.1 download page could land on an empty copy.

## 0.1.1

- Fixed the app failing to start after install with "Cannot find module ./settings".

## 0.1.0

- First release: the scale camera full screen with the live weight and the plate read off each truck as overlays. Camera found over ONVIF, scale read through the NPort, plates read on the PC, every weighing photographed into a local history with a chosen retention.
