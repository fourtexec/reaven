# Android app

`android/` is a Kotlin + Jetpack Compose **operator console**. It contains no
agent code: it opens the same operator connection the CLI does, and everything it
can do requires a running agent on the machine being controlled.

## Verified build

This exact APK was built and inspected with:

| Component | Version |
|---|---|
| JDK | 21 (`openjdk 21.0.12`) |
| Gradle | 8.7 |
| Android Gradle Plugin | 8.5.2 |
| Kotlin | 2.0.20 (Compose compiler plugin) |
| Compose BOM | 2024.09.02 |
| compileSdk / targetSdk | 34 (Android 14) |
| minSdk | 26 (Android 8.0) |
| Build tools | 34.0.0 |

Output: `android/artifacts/reaven-control-0.2.0-debug.apk`, 16 MB, package
`dev.reaven.control`, permissions `INTERNET` and `ACCESS_NETWORK_STATE` only.

```bash
adb install android/artifacts/reaven-control-0.2.0-debug.apk
```

## Building from source

```bash
cd android
gradle wrapper --gradle-version 8.7   # once, if ./gradlew is missing
./gradlew assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

Or open the folder in Android Studio (Koala or newer) and press Run. Gradle needs
`JAVA_HOME` pointing at a JDK 17–21 and an Android SDK with platform 34 + build
tools 34.0.0 installed.

Release signing is not configured — add a signing config to `app/build.gradle.kts`
before shipping a release build.

## Architecture

```
MainActivity.kt        app state machine: Connect → Machines → Device
data/Protocol.kt       JSON builders + frame parser (mirrors src/protocol.rs)
data/ReavenClient.kt   socket, frame dispatch, request/response futures, streams
ui/ConnectScreen.kt    relay host/port/password, persisted in SharedPreferences
ui/DevicesScreen.kt    machine list, refreshed every 3 s
ui/DeviceScreen.kt     Scaffold + tabs
ui/ScreenTab.kt        live stream + touch-to-mouse + keys + snapshot
ui/ConsoleTab.kt       command console (one exec per command)
ui/CameraTab.kt        remote webcam list, live view, single frame
util/Gallery.kt        saves PNGs to MediaStore (no storage permission needed)
```

`ReavenClient` keeps one socket per operator session. A single reader coroutine
parses each line and routes it by `type`: responses resolve a
`CompletableDeferred` keyed by request id, video frames go to a `SharedFlow`
(dropping the oldest under backpressure rather than queueing), shell output and
exits go to their own flows.

### Touch mapping

`ScreenTab` normalises pointer positions against the **letterboxed image rect**,
not the composable bounds:

```
shownW/H = fit(frameAspect into containerAspect)
nx = (touch.x - (container.w - shownW)/2) / shownW
```

so a 20:9 phone can drive a 16:9 desktop without offset error. Values are
clamped to `0..1`; the agent clamps again.

### Gestures

| Gesture | Sent |
|---|---|
| tap | `click left` |
| press, hold >500 ms, release | `click right` |
| drag | `down left` → `move`* → `up left` |
| ▲ / ▼ buttons | `scroll ∓2` |

Drag is detected after a 14 px slop so a slightly wobbly tap is still a click.

## Limitations

- **Console is not a terminal.** One `exec` per command; ANSI colour is not
  rendered. Use `reaven cl` on a desktop for a real PTY.
- **No audio, no clipboard sync, no file browser** (the protocol supports
  upload/download; the app does not expose it yet).
- **Snapshot saves the *displayed JPEG***, so its resolution equals the stream's
  `max_width` (1600 px), not the monitor's native size. For a full-resolution
  capture, use `reaven screenshot` or add an explicit `screenshot` call — the
  protocol already supports it.
- **Portrait/landscape**: the stream re-fits automatically, but the control bar
  is optimised for portrait.
- **Password stored in plain SharedPreferences**, and cleartext TCP is allowed.
  Fine on a trusted LAN, not fine on a shared phone or public network.
- Frames are decoded on `Dispatchers.Default`; on very old devices a 1600 px
  stream at 15 fps will drop frames (backpressure drops the oldest, so the app
  stays responsive).
