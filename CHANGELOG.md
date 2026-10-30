# Changelog

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
