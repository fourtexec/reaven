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
