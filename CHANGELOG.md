# Changelog

Newest first. The app shows this file on the Updates tab of its settings.

## 0.1.14

- Only one copy of the app runs at a time. Opening it again while it is running brings the open window forward. Before, a second copy read the same scale and every truck was logged twice.
- When an update check fails, a "Copy error" button on the Updates tab copies the whole error, to paste into a message. A check made in the few minutes while a new version is still being published now says so, instead of "Cannot find latest.yml".
- A Reconnect button at the top right of the picture connects to the camera again. Use it when the picture comes up gray and blocky, which happens now and then when the live view connects.
- The live view writes what it is doing to the log: what the camera sends, how the video conversion is keeping up, and once a minute what the picture in the window is showing. A bug report about the picture can then say where it goes wrong. Everything the video conversion reports about the camera connection is in the log too.
- Dealer placards and stickers in the plate holder are no longer read as plates. A read with no digit is taken for a sign unless "A plate must contain a digit" is turned off on the Plates tab, words can be listed there to ignore, and the same text found twice in one picture (a placard on each mud flap) is never a plate.
- The Changelog tab is now the Updates tab. Once an update is downloaded, its button becomes "Restart to update".
- The plate reader no longer needs Python. The same plate models now run in a small built-in reader, which reads the same plates, starts in under a second and is a little quicker per read. The installer is about a fifth smaller (247 MB, from 316 MB), and the installed app 240 MB smaller.

## 0.1.13

- Two update channels, chosen on the Updates tab. Stable, the default, takes releases only. Beta also takes pre-release builds as they are published, for the one PC that tries things first, and goes back to the stable release when switched back. A button opens the releases page for installing a build by hand.
- A truck still rocking on the deck is no longer taken as settled on indicators that send no line endings: the motion flag at the end of each reading was being cut off.
- With "Save log" off, saved photos are kept for the retention period. Before, every photo was deleted when the app started and every hour after.
- The live view no longer freezes now and then when it is opened: a viewer could start partway through a frame and the player stalled on it.
- Weighing photos are taken faster, about 1.6 s from the camera instead of 2.5 s, and never from a half-drawn frame on cameras that start their stream between whole pictures.
- Camera errors say what the camera said, such as "Invalid username or password", instead of just "SOAP fault".

## 0.1.12

- The window opens where it was last closed, at the same size, maximised if it was. If that spot is on a screen that is no longer connected, it opens at the default size on the main screen.

## 0.1.11

- Bug reports go through a relay built into the app, so a new yard can send one with nothing set up; the GitHub token lives in the relay, not in the installer. The token field on the Bug report tab is now optional.

## 0.1.10

- A bug report still goes through when the repository lacks the label the app puts on it.

## 0.1.9

- A Bug report tab in Settings. Type what happened and press "Send to GitHub": the report goes up as an issue with the app's version, its settings (passwords removed), the last 40 weighings and the recent log, so the problem can be looked at without anyone coming to the PC. It needs a GitHub token entered once on that tab. With no internet, "Save report to a file" writes the same report to send along later.

## 0.1.8

- The same truck is no longer weighed again and again while it sits on the deck. The deck now has to read clear for the whole settle window before the next weighing can fire, so a single stray low reading from the indicator (a corrupted frame, a glitch on the line) is not taken as the truck leaving. Saving settings while a truck is on the deck no longer weighs it again either.
- The app's log is written to station.log beside the settings file, kept to about 2 MB, so what the scale and camera did can be read back later.

## 0.1.7

- A truck that settles with only part of itself on the deck no longer leaves that partial weight in the history. If the scale settles again heavier than the weight already taken, by more than the wobble tolerance, the same history entry is corrected: it takes the new weight and a fresh picture of the whole truck, keeps its plate, and notes the weight first read. This repeats as more of the truck comes on, a trailer for instance, and a lighter settle (the truck rolling off) is never taken as a correction.

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
