# Runtime requirements (using it, not building it)

## TL;DR

**Nothing to install.** The shipped binary is statically linked: it pulls in no
shared libraries at all, so no `libxcb`, no `libX11`, no `libdbus`, no `libxdo`,
no `libv4l`, and no glibc-version dependency.

```bash
chmod +x artifacts/reaven-0.2.0-linux-x86_64
./artifacts/reaven-0.2.0-linux-x86_64 --version
```

```
$ ldd artifacts/reaven-0.2.0-linux-x86_64
        statically linked
```

For the Android app: **nothing.** Android 8.0+ and a network connection.

## Why this matters

Screen capture used to be built on `xcap`, which links `libxcb.so.1` and
`libdbus-1.so.3`. Because the dynamic loader resolves those *before `main()`
runs*, a machine without them could not start the binary at all — so even
`reaven exec`, which never touches a screen, died with:

```
reaven: error while loading shared libraries: libxcb.so.1: cannot open shared object file
```

Capture is now implemented directly on `x11rb`, which speaks the X protocol over
the existing socket and needs no C library. Two consequences:

* command-line use works on a machine with **no display and no X libraries**;
* desktop commands fail with a message instead of preventing startup:

```
$ reaven screenshot --server host --target pc
Error: connecting to the X server (is DISPLAY set? under Wayland, capture needs
XWayland): $DISPLAY variable not set and no value was provided explicitly
```

## Per-feature requirements

| Feature | Needs |
|---|---|
| relay, `exec`, `cl`, upload/download | nothing — works headless |
| screenshot / screen stream | an X11 display (`DISPLAY`), or XWayland on Wayland |
| mouse + keyboard control | same as above |
| webcam | a `/dev/video*` device + membership of the `video` group |
| browser viewer | any browser on the machine you are sitting at |
| `install-autostart` | `flock` from util-linux |

Fast check before blaming Reaven:

```bash
echo "$DISPLAY"                 # empty -> no X session for this process
xdpyinfo | head -5              # can we talk to the display at all?
id -nG | tr ' ' '\n' | grep video   # webcam permission
ls /dev/video*                  # webcam devices
```

## Wayland

Capture and control run through **XWayland**. That covers X11 applications,
which is most of them, but:

* a native Wayland window may be missing from the capture;
* clicks and keys only reach XWayland clients, not native Wayland ones.

Native-Wayland capture needs an `xdg-desktop-portal` + PipeWire backend, which
this build does not include — that backend is exactly what used to introduce the
`libdbus`/`libxcb` dependency. If you need it, the old `xcap` path can be
restored as an optional feature.

## What the watched machine shows

**Nothing.** There is no banner, border, tray icon, cursor highlight, flash or
animation when another device views the screen. Capture is a bare framebuffer
read (`xproto.get_image`) over the X socket, and the agent never creates a
window, so there is nothing on screen to suppress or turn off — X11 has no
"someone is capturing" notification for a client to trip in the first place.

This is asserted by the test suite rather than assumed: while a stream is
running, `tests/e2e.sh` checks that the watched display's window tree and its
pixels are identical to before (currently `windows=3, diff=0`).

Where an indicator *would* come from:

| Situation | What appears |
|---|---|
| X11 or XWayland capture (this build) | nothing |
| native-Wayland capture via the desktop portal | the compositor's own "screen is being shared" indicator. GNOME and KDE draw it, not the application, and no application can switch it off. This build does not use the portal — see [Wayland](#wayland). |
| Android | nothing: the app never touches `MediaProjection`, so no cast icon appears on the phone |

One honest limit: this is about the *screen*, not about being undetectable.
Anyone who can log in to the watched machine can still see that it is being
accessed — the agent is a process in `ps`, it writes every stream, shell command
and rejected input to its local log on purpose, and there is traffic to the
relay. A server you administer should not be a machine that silently watches its
users, so the log is not something to remove.

## Troubleshooting

### `screen capture failed: the X server refused GetImage …`

The agent asked the X server for the framebuffer and was refused. The message now
names the rectangle it asked for, the root window size, the depth and the X error,
which is usually enough to tell these apart:

| X error | Usual cause | Handled how |
|---|---|---|
| `BadMatch` | the monitor rectangle does not fit inside the root window: fractional display scaling, mixed-DPI multi-head, or a layout left over from a mode change | the rectangle is clamped to the root; if the direct request still fails, the whole root is captured and the monitor is cropped out of it |
| `BadValue`/`BadMatch` on a 30-bit deep-colour screen | the plane mask asked for bits past the root depth | retried automatically with a mask sized to the depth |
| `BadAccess`/`BadDrawable` | another client grabbed the server, or the display was reconfigured mid-request | the stream keeps running; the next frame retries |
| any error right after the display wakes | DPMS blanking | recovers on the next frame |

A locked screen is rarely the cause on X11 — lockers normally leave the root
viewable. A Wayland session that does not expose the root pixmap *is* a cause:
capture there needs XWayland and an X11-managed window, see [Wayland](#wayland).

Every agent now runs a capture self-test at start-up, so this is visible before
anything goes wrong:

```
Screen capture self-test:
  monitor 0 'HDMI-1' 2560x1440 at 0+0 -- capture ok (2560x1440 frame, 184231 bytes JPEG)
```

A `capture FAILED` line names the monitor that is a problem. It is not fatal:
shell, file transfer and input keep working, and during a stream repeated identical
errors are logged once every 10 seconds rather than once per frame.

### Nothing else?

Run `reaven screens` against the agent — it prints exactly what the agent believes
the display looks like, which is the fastest way to spot a scaling or layout
mismatch.

## Android

| Requirement | Value |
|---|---|
| Android version | 8.0 (API 26) or newer |
| Permissions | `INTERNET`, `ACCESS_NETWORK_STATE` only |
| Play Services / extra libs | none |
| Storage permission | none (saves via MediaStore) |

```bash
adb install android/artifacts/reaven-control-0.2.0-debug.apk
```

It is a **debug** build — fine to sideload, but not release-signed, so Play
Protect will ask for confirmation.

## Building it yourself

If you would rather build (or need a different architecture):

```bash
cargo build --release                                       # dynamic, ~5 MB
cargo build --release --target x86_64-unknown-linux-musl    # static, ~3.9 MB
```

The only build-time packages that matter are `libclang-dev` (for the webcam
v4l2 bindings) and `pkg-config`. Drop webcam support and you need neither:
`cargo build --release --no-default-features --features screen,input`.
