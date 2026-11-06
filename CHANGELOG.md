# Changelog

## Unreleased — WSS transport and automatic TLS

### Added
- **WSS transport.** Relay, agents, CLI operators and the Android app now speak
  TLS-protected WebSocket instead of plaintext TCP. One JSON frame is one
  WebSocket text message; ping/pong and close are handled at the transport layer.
  `MAX_FRAME_BYTES` (32 MB) is enforced at both the protocol and WebSocket layers.
  Implementation: `src/transport.rs`.
- **Automatic Let's Encrypt certificates** via `--acme-domain` (and optional
  `--acme-email`, `--acme-cache`, `--acme-staging`). Uses ACME TLS-ALPN-01, so
  validation and the relay share TCP 443 and no HTTP challenge server is needed.
  Certificates are cached in `~/.reaven/acme` and renewed in the background.
  Implementation: `src/tls.rs`.
- **Existing PEM certificates** with `--tls-cert`/`--tls-key`.
- **Self-signed local mode** with `--self-signed`, plus a global `--insecure` flag
  and `REAVEN_INSECURE=true` for clients connecting to it.
- Client `--server` accepts `host`, `host:port`, `https://host`, `wss://host/...`
  and full `wss://host/path` URLs. Shorthand values are normalized to
  `wss://host[:port]/ws`.
- Android: OkHttp 4.12.0 WebSocket client over WSS, self-signed certificate
  toggle on the Connect screen, default port 443, cleartext traffic disabled.

### Changed
- **Breaking:** the relay has no plaintext listener. `reaven server` now requires
  one TLS mode (`--acme-domain`, `--tls-cert`/`--tls-key`, or `--self-signed`) and
  defaults to `--listen 0.0.0.0:443`.
- Protocol framing changed from newline-delimited JSON over raw TCP to one JSON
  object per WebSocket message. v2 peers that predate this change cannot connect.
- TLS uses `rustls` with the `ring` backend; no OpenSSL runtime dependency.
- Android manifest no longer permits cleartext traffic.

### Security
- Passwords, shell traffic, screen frames and webcam frames are encrypted in
  transit and protected against relay impersonation when certificate validation
  is enabled. `--insecure` / **Allow self-signed certificate** turns that
  authentication off and must be limited to local/self-signed relays.
- TLS terminates at the relay, which remains a trusted component and can still
  observe or alter routed frames. The shared-password model is unchanged.

### Documented
- Updated README, `docs/PROTOCOL.md`, `docs/RUNTIME.md` and `docs/ANDROID.md` for
  WSS, TLS modes, ACME port-443 requirements, `--insecure`, and the rebuilt
  Android client. Noted that the checked-in 0.2.0 binary and APK predate WSS and
  must be rebuilt for the secure transport.

### Testing
- Unit tests cover URL normalization and frame formats.
- `tests/e2e.sh` and `tests/android_client_sim.py` now run against a self-signed
  WSS relay with `REAVEN_INSECURE=true`; the Python simulator performs its own TLS
  and WebSocket handshake.

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
