---
name: runode-simulator
description: Run, test and operate apps on an iOS simulator or Android emulator while the user watches it in runode's simulator page - build and install the app, tap, swipe, type, read the screen and its UI tree with mobilecli. Use it whenever you run inside a runode terminal (RUNODE_SESSION is set) and the task touches a simulator, an emulator or a mobile app on one - "test the iOS app", "try it on the simulator", "check the Android build", a UI flow to click through, a screenshot of the app - even if the project has its own script that opens Simulator.app.
---

# Simulators and emulators in runode

runode's window has a simulator page on its right: it shows a live picture of
an iOS simulator, Android emulator or connected phone and lets the user tap on
it. runode does not drive devices itself; you use
[mobilecli](https://github.com/mobile-next/mobilecli) (`npm i -g mobilecli`),
which prints JSON, on the same device, and the user sees every tap you make.

## Before you start

Ask the user to open the simulator page (View > Simulator, cmd-shift-M by
default) and to pick or boot the device there; you cannot open the page from
the command line. Then find the device's id:

```sh
mobilecli devices          # booted devices: id, name, platform, type
```

Do not open Simulator.app (`open -a Simulator`) or start a second emulator
window, and do not run a project script that does: the page already shows the
device, and two viewers fight over it. If a project script does more than
build and install, do its build step yourself and install into the device that
is already booted:

```sh
xcodebuild -project App.xcodeproj -scheme App -configuration Debug \
  -destination "platform=iOS Simulator,id=$id" -derivedDataPath build/dd build
xcrun simctl install "$id" build/dd/Build/Products/Debug-iphonesimulator/App.app
xcrun simctl launch --terminate-running-process "$id" com.example.app

adb -s "$id" install -r app/build/outputs/apk/debug/app-debug.apk
```

Long builds go in a pane of their own (`runode open --right`, then `runode
send ... --enter --wait`, see the runode skill) so the user can watch them.

## Drive the device

```sh
mobilecli dump ui --device <id>                     # elements with their rects
mobilecli snapshot --device <id>                    # the same as text
mobilecli screenshot --device <id> --output /tmp/screen.png   # then read the image
mobilecli io tap --device <id> 120,640              # screen points on iOS, pixels on Android
mobilecli io longpress --device <id> 120,640
mobilecli io swipe --device <id> 200,700,200,200
mobilecli io text --device <id> 'hello'             # types into the focused field
mobilecli io button --device <id> HOME              # BACK on Android only
mobilecli url --device <id> 'myapp://path'          # open a deep link
mobilecli apps launch --device <id> com.example.app
mobilecli device logs --device <id> --filter process=App --limit 100
```

Find what to tap in `dump ui` and tap the centre of its rect rather than
guessing from a screenshot; after each step dump or screenshot again to check
that the screen changed the way you expected, and report what you saw.

The first command on an iOS simulator may fail with "agent is not installed":
run `mobilecli agent install --device <id>` once. A real iPhone needs a
provisioning profile for that step; ask the user instead of guessing one.

## Be careful

- The device and its apps belong to the user. Erasing a simulator,
  uninstalling an app or wiping its data loses their state; ask first.
- An app on the device may talk to real services, including the runode on this
  Mac: actions in it (opening terminals, committing, pairing) happen for real.
  Say what you are about to do when it reaches beyond the device.
