# Changelog

Newest first. The app shows this file on the Updates tab of its settings.

## 0.2.1

- The Bug report tab takes ideas as well as problems: pick Problem or Idea at the top. An idea goes up marked as one, so requests are kept apart from things that are broken.

## 0.2.0

- The plate is read from the moment a truck rolls onto the scale, not only once it has settled: a truck that pulls up too far for its plate to be seen is still read on the way in. Pictures are taken from the full-resolution stream about once a second until the truck leaves. The plate kept is the one read most surely across all of them, so one misread close-up does not win over several agreeing reads.
- The saved photo is the full-resolution picture in which the plate was largest: the truck as close as it came while its plate could still be read. It is replaced, stamp and all, whenever a closer picture or a surer plate comes in, so the plate on the photo is always the plate in the history. "Pictures per truck" and "Seconds between pictures" are on the Plates tab (up to 90, one second apart; it stops sooner once the truck has settled and the plate is sure).
- The photo quality setting is now used for saved photos.
- The live picture zooms: the mouse wheel zooms in or out about the pointer, dragging a box zooms into it (widened or heightened to the screen's shape), and right-click shows the whole picture again. The weight, plate and clock stay where they are. Zoomed past 1.5 times, the picture switches to the full-resolution stream so it stays sharp. The picture carries on while the other stream connects, and changes over once it is playing.
- The live picture no longer goes black while it reconnects, or comes up gray for a moment after: the old picture stays until the new one is playing, and the new one starts at the camera's first whole picture.
- The history's full-size view shows the newest photo and plate of a weighing that changed after it was opened.
- The app is rebuilt in Rust. It looks and works the same. Installing it takes the old app off the PC and carries on from its settings: the camera and scale, the window's place, the log and the history and its photos all stay as they were.
- The plate comes from the truck on the scale. Every plate in the picture is read and the largest is taken, since the truck on the deck is nearest the camera; plates toward the right of the picture count for more. Both are set on the Plates tab: the largest plate or the clearest read, which side to favour (right, left or neither) and by how much. There is no longer a vehicle finder to mistake a truck for a train.
- Updates carry on from where a dropped download stopped, instead of starting again. A check that fails is tried again within minutes.
- Going back to the old app is done by hand: Settings, Updates, Open the releases page, and install 0.1.15.

## 0.1.15

- The plate is read off the truck on the scale, not one parked in the yard behind it. A truck seen head-on with its headlights on at night can be taken for a train by the vehicle finder, and trains were not counted as vehicles, so the photo picked the next vehicle it saw.
- Only one copy of the app runs at a time. Opening it again while it is running brings the open window forward. Before, a second copy read the same scale and every truck was logged twice.
- When an update check fails, a "Copy error" button on the Updates tab copies the whole error, to paste into a message. A check made in the few minutes while a new version is still being published now says so, instead of "Cannot find latest.yml".
- The live view no longer comes up gray and blocky. When the camera's frame rate could not be read on connecting, the picture was encoded as if at 90,000 frames a second, leaving almost nothing for each frame. It is now always sent at 15 frames a second, which also takes less of the PC's time. A Reconnect button at the top right of the picture connects to the camera again, should it ever be needed.
- The live view no longer drifts behind the yard. Every hiccup used to add a little delay that was never made up, until people were seen walking in after they had come through the door. The picture now plays a little fast until it is back to the moment, and reconnects if it falls more than 8 seconds behind. Each frame is also sent to the window as soon as it is ready, instead of two seconds at a time, so the picture runs well under a second behind the camera's own feed, from about two seconds before.
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
