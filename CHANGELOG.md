# Changelog

## Unreleased — capture failures are diagnosable, and often recoverable

### Fixed
- **`screen capture failed: X server refused the capture` was unactionable and
  often avoidable.** Capture now clamps the requested rectangle to the root window,
  retries the plane mask for deep-colour screens, and — if the direct request is
  still refused — captures the whole root and crops the monitor out of it. That
  last fallback fixes the common case where RandR reports logical monitor
  rectangles that do not match the root pixmap (fractional scaling, mixed-DPI
  multi-head).
- The error message now names the rectangle, root size, depth and the X error, and
  lists the likely causes instead of guessing "screen locked?".
- Repeated identical capture failures during a stream are logged once per 10
  seconds instead of once per frame.
- `crop` bounds check was wrong for rectangles that start mid-row, which would have
  rejected legitimate crops of a non-leftmost monitor in the full-root fallback.

### Added
- Agents print a **capture self-test at start-up** — every monitor with its
  geometry and whether a frame can actually be grabbed. A failure is reported but
  not fatal: shell, files and input keep working.
- Troubleshooting section in `docs/RUNTIME.md` mapping X errors to causes.
- 7 unit tests for the geometry clamping and cropping (20 unit tests total).




## 0.2.1 — no runtime dependencies

### Fixed
- **Command-line use no longer depends on X11 libraries.** Screen capture was
  built on `xcap`, which links `libxcb.so.1` and `libdbus-1.so.3`. The dynamic
  loader resolves those before `main()`, so on a machine without them *every*
  command died at startup — including `reaven exec`, which never touches a
  screen. Capture is now implemented directly on `x11rb` (pure Rust, speaks the
  X protocol over the socket), so the binary links no C libraries at all.
- Desktop commands now fail with an actionable message at call time
  (`is DISPLAY set? under Wayland, capture needs XWayland`) instead of aborting
  the process.

### Added
- Fully static build: `cargo build --release --target x86_64-unknown-linux-musl`
  produces a 3.9 MB binary with zero runtime dependencies and no glibc version
  requirement. Shipped as `artifacts/reaven-0.2.0-linux-x86_64`, replacing the
  two dynamic binaries.
- Pure-Rust X11 capture: RandR 1.5 monitor enumeration (with a root-geometry
  fallback for headless servers), ZPixmap -> RGBA conversion covering 16/24/32
  bits per pixel in both byte orders.
- e2e test that paints a red->blue gradient and asserts channel order and
  vertical orientation of the capture, guarding the new backend against
  channel swaps.

### Changed
- Native-Wayland capture is no longer included; XWayland is used instead. The
  portal/PipeWire path was the source of the `libdbus` dependency. See
  `docs/RUNTIME.md`.

### Documented
- The watched machine shows **nothing** when its screen is viewed — no banner,
  border, tray icon or animation — because capture is a bare framebuffer read and
  the agent never creates a window. New e2e check asserts the window tree and
  pixels are unchanged during a stream (`diff=0`), so an on-screen overlay cannot
  be added later without failing the suite. See "What the watched machine shows"
  in `docs/RUNTIME.md`, including the cases where an indicator is the
  compositor's own and cannot be suppressed.

### Verified
- 26 end-to-end checks pass against **both** the dynamic and the static binary.
- Statistics on the static build: 20 fps sustained at native 1440x900 (requested
  20 fps), 30 frames in a 3 s window during the e2e run.


## 0.2.0 — screen, webcam, input control, Android client

Building on the upstream 0.1 relay/shell/file-transfer prototype.

### Added — agent capabilities
- `screenshot` operation: one-shot JPEG of any monitor, with `--quality` and `--max-width`.
- `list_screens` / `list_cameras` operations to enumerate monitors and webcams.
- `camera_snapshot` operation: one-shot JPEG from a webcam.
- `video_start` / `video_frame` / `video_stop` frames: live MJPEG streaming from a
  monitor or webcam, with per-stream id, sequence numbers and adjustable fps/quality/scale.
- `control` frames: mouse move/down/up/click/double-click/scroll and
  key/text/key-combo injection. Coordinates are normalised `0.0..=1.0` against the
  streamed frame, so the controlling device never needs to match the remote resolution.
- `--no-input` agent flag for a view-only agent; input is refused with a clear error.
- `peers` / `list_peers`: online-machine discovery, filtered to real agents so
  operators never appear as machines.

### Added — operator tooling
- `reaven screenshot` — grab a JPEG to a local file.
- `reaven screens` / `reaven cameras` — enumerate monitors and webcams.
- `reaven screen` — live screen stream plus a built-in browser viewer that can also
  drive the mouse and keyboard (MJPEG over HTTP, `POST /input` for events).
- `reaven webcam` — live webcam stream, or `--snapshot` for a single frame.
- `reaven input` — inject clicks, scrolls, keys, text and chords from the CLI.
- `--record DIR` writes every received frame to disk; `--duration N` for scripted runs.

### Added — Android app (`android/`)
Kotlin/Compose operator console: device list, live screen with touch-to-mouse,
snapshot capture to the gallery, command console, and webcam viewer.

### Changed
- Protocol v2. `Frame::Response.result` is now an untagged `ResponseResult`
  (payload object on success, bare string on failure) rather than serde's default
  `{"Ok": …}` wrapping, which was hostile to non-Rust clients.
- `Frame::Hello` gains optional `kind` and `info` fields; older peers still parse.
- Split the single 1055-line `main.rs` into `protocol` / `relay` / `agent` /
  `operator` / `desktop` / `httpd` modules.
- Removed the stale prebuilt `reaven` binary from the repository root; build from source.

### Fixed
- Agents no longer block their read loop while capturing a frame: snapshot and
  exec work is dispatched to blocking tasks, keeping shell and video responsive.
- Capture failures on a single frame (screen lock, resolution change) no longer
  kill an active stream.
