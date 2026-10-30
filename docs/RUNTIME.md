# Runtime requirements (using it, not building it)

## TL;DR

For normal desktop use: **nothing to install.** The binary needs only libraries
every desktop Linux already has.

For the Android app: **nothing.** Android 8.0+ and a network connection.

## What is actually required

Measured from the shipped binary (`objdump -p`), not guessed:

| Linked library | Needed for | Present by default? |
|---|---|---|
| `libc.so.6`, `libm.so.6`, `libgcc_s.so.1` | everything | yes |
| `libxcb.so.1` | screen capture / input on X11 | yes on any GUI install |
| `libdbus-1.so.3` | Wayland portal capture, session bus | yes on any desktop |

Not required: `libxdo` (this build uses enigo's pure-Rust X11 backend),
`libpipewire`, `libv4l`/`libv4l2` (the webcam backend statically links its own
V4L2 layer), GTK, Qt, Python, Node, or any runtime.

### Per-feature extras

| Feature | Needs |
|---|---|
| relay, `exec`, `cl`, upload/download | nothing beyond the above |
| screenshot / screen stream | an X11 session (`DISPLAY`), or `xdg-desktop-portal` on Wayland |
| mouse + keyboard control | X11; on Wayland the compositor must support libei |
| webcam | a `/dev/video*` device and membership of the `video` group (`sudo usermod -aG video $USER`) |
| browser viewer | any browser; no server-side deps |
| `install-autostart` | `flock` from util-linux (present on virtually every distro) |

## Two prebuilt binaries in `artifacts/`

| File | Size | What it is |
|---|---|---|
| `reaven-0.2.0-linux-x86_64` | 5.1 MB | Full build: relay, shell, screen, webcam, input. Dynamically linked. |
| `reaven-0.2.0-relay-linux-x86_64-static` | 3.0 MB | Relay-only build (`--no-default-features`). **Statically linked — zero runtime dependencies**, runs on any x86-64 Linux including Alpine. |

The static build has no screen/webcam/input code at all. If you point it at an
agent it answers `this build has no screen support`, which is the intended
behaviour — use it for the relay on a VPS or a minimal server.

```bash
./artifacts/reaven-0.2.0-relay-linux-x86_64-static server \
    --listen 0.0.0.0:7331 --password 'a-long-random-secret'
```

## Two caveats worth knowing

**1. The full binary needs glibc 2.39+.** It was built on Debian 13. That covers
Debian 13, Ubuntu 24.10+/25.04+, Fedora 40+, Arch, and similar. On Ubuntu 22.04
(glibc 2.35), Debian 12 (2.36) or RHEL 9 you will get
`version 'GLIBC_2.39' not found`. Two options:

```bash
# a) build it yourself — one command, ~2 minutes
cargo build --release
# b) or install the two system libs you're missing and use the static relay build
sudo apt install libdbus-1-3 libxcb1     # only if the full binary won't start
```

**2. A headless server may not start the full binary at all.** Dynamic linking
happens before `main()` runs, so a bare VPS without `libdbus-1-3`/`libxcb1`
fails at startup even if you only want the relay. That is exactly what the
static relay-only build is for.

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

It is a **debug** build — fine to sideload, but it is not release-signed, so
expect Play Protect to ask for confirmation.
