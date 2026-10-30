//! Desktop backends: monitor capture, webcam capture, and input injection.
//!
//! Each capability sits behind a Cargo feature (`screen`, `webcam`, `input`)
//! so the relay/agent still builds on machines without the native
//! dependencies. When a feature is off, the corresponding entry point returns
//! an explanatory error instead of failing to compile.
//!
//! All capture entry points are deliberately *thread-scoped*: the underlying
//! handles (X11 connection, V4L2 device) are not `Send` on every platform, so
//! they are opened and used inside the thread that streams from them.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::protocol::InputEvent;

/// A JPEG frame plus its pixel dimensions.
pub struct EncodedFrame {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MonitorInfo {
    pub index: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub primary: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

// ---------------------------------------------------------------- encoding ---

#[cfg(any(feature = "screen", feature = "webcam"))]
fn encode_jpeg(
    raw: &[u8],
    width: u32,
    height: u32,
    max_width: u32,
    quality: u8,
    color: jpeg_encoder::ColorType,
) -> Result<EncodedFrame> {
    use image::imageops::FilterType;

    if width == 0 || height == 0 {
        bail!("captured an empty frame");
    }
    let quality = quality.clamp(1, 100);

    // Down-scale only when the caller asked for a cap and the frame is wider.
    let (target_w, target_h) = if max_width > 0 && width > max_width {
        let scale = max_width as f32 / width as f32;
        (max_width, ((height as f32 * scale).round() as u32).max(1))
    } else {
        (width, height)
    };

    let pixels: Vec<u8> = if (target_w, target_h) != (width, height) {
        let channels = match color {
            jpeg_encoder::ColorType::Rgba => 4,
            _ => 3,
        };
        let expected = width as usize * height as usize * channels;
        if raw.len() < expected {
            bail!("frame buffer is {} bytes, expected {expected}", raw.len());
        }
        let dynamic: image::DynamicImage = if channels == 4 {
            image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(width, height, raw[..expected].to_vec())
                    .ok_or_else(|| anyhow!("malformed RGBA buffer"))?,
            )
        } else {
            image::DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(width, height, raw[..expected].to_vec())
                    .ok_or_else(|| anyhow!("malformed RGB buffer"))?,
            )
        };
        let resized = dynamic.resize_exact(target_w, target_h, FilterType::Triangle);
        match color {
            jpeg_encoder::ColorType::Rgba => resized.to_rgba8().into_raw(),
            _ => resized.to_rgb8().into_raw(),
        }
    } else {
        raw.to_vec()
    };

    let mut jpeg = Vec::with_capacity(64 * 1024);
    let encoder = jpeg_encoder::Encoder::new(&mut jpeg, quality);
    encoder
        .encode(&pixels, target_w as u16, target_h as u16, color)
        .context("JPEG encoding failed")?;
    Ok(EncodedFrame {
        jpeg,
        width: target_w,
        height: target_h,
    })
}

// ------------------------------------------------------------------ screens ---

/// X11 capture built directly on `x11rb`.
///
/// `x11rb` speaks the X wire protocol over the socket itself, so the binary
/// links **no C libraries** -- no `libxcb`, no `libX11`, no `libdbus`. That
/// matters two ways:
///
///   1. a missing X library used to make the whole binary fail at load time
///      (the loader runs before `main`), so even `reaven exec` on a headless
///      server died with "libxcb.so.1: cannot open shared object file";
///   2. it makes a fully static build possible.
///
/// Failures now happen at call time, with a message, only for the commands
/// that actually need a display.
#[cfg(feature = "screen")]
mod x11 {
    use anyhow::{anyhow, bail, Context, Result};
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window};
    use x11rb::rust_connection::RustConnection;

    use super::{encode_jpeg, EncodedFrame, Geometry, MonitorInfo};

    pub struct Screen {
        conn: RustConnection,
        root: Window,
        pub monitors: Vec<MonitorInfo>,
        bits_per_pixel: u8,
        lsb_first: bool,
        root_width: u32,
        root_height: u32,
        root_depth: u8,
    }

    pub fn open() -> Result<Screen> {
        let (conn, screen_num) = x11rb::connect(None).context(
            "connecting to the X server (is DISPLAY set? under Wayland, capture needs XWayland)",
        )?;
        let setup = conn.setup();
        let root = setup.roots[screen_num].root;
        let depth = setup.roots[screen_num].root_depth;
        let bits_per_pixel = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == depth)
            .map(|f| f.bits_per_pixel)
            .unwrap_or(32);
        let lsb_first = setup.image_byte_order == ImageOrder::LSB_FIRST;
        let monitors = query_monitors(&conn, root, screen_num)?;
        let root_width = setup.roots[screen_num].width_in_pixels as u32;
        let root_height = setup.roots[screen_num].height_in_pixels as u32;
        Ok(Screen {
            conn,
            root,
            monitors,
            bits_per_pixel,
            lsb_first,
            root_width,
            root_height,
            root_depth: depth,
        })
    }

    fn query_monitors(
        conn: &RustConnection,
        root: Window,
        screen_num: usize,
    ) -> Result<Vec<MonitorInfo>> {
        let mut out = Vec::new();

        // RandR 1.5 exposes the logical monitor layout, which is what a user
        // means by "my screen" (it also gets HiDPI scaling right).
        if let Ok(cookie) = conn.randr_get_monitors(root, true) {
            if let Ok(reply) = cookie.reply() {
                for (index, m) in reply.monitors.iter().enumerate() {
                    let name = conn
                        .get_atom_name(m.name)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                        .unwrap_or_else(|| format!("display {index}"));
                    out.push(MonitorInfo {
                        index: index as u32,
                        name,
                        x: m.x as i32,
                        y: m.y as i32,
                        width: m.width as u32,
                        height: m.height as u32,
                        scale: 1.0,
                        primary: m.primary,
                    });
                }
            }
        }

        // No RandR (headless Xvfb, ancient server): treat the whole root as one
        // monitor rather than reporting "no monitors".
        if out.is_empty() {
            let screen = &conn.setup().roots[screen_num];
            out.push(MonitorInfo {
                index: 0,
                name: "screen".to_string(),
                x: 0,
                y: 0,
                width: screen.width_in_pixels as u32,
                height: screen.height_in_pixels as u32,
                scale: 1.0,
                primary: true,
            });
        }
        if !out.iter().any(|m| m.primary) {
            if let Some(first) = out.first_mut() {
                first.primary = true;
            }
        }
        Ok(out)
    }

    impl Screen {
        pub fn capture(
            &self,
            index: u32,
            max_width: u32,
            quality: u8,
        ) -> Result<(EncodedFrame, Geometry)> {
            let monitor = self.monitors.get(index as usize).ok_or_else(|| {
                anyhow!("monitor {index} does not exist; run `reaven screens` to list")
            })?;
            let (x, y, width, height) = self.fit_to_root(monitor)?;

            // Direct capture of the monitor's rectangle. If the X server refuses
            // -- typically BadMatch, because RandR's monitor rectangles are in
            // logical coordinates while the root pixmap is in device pixels
            // (fractional scaling, mixed-DPI multi-head) -- fall back to grabbing
            // the whole root, which is always inside the drawable, and crop.
            let rgba = match self.grab(x, y, width, height) {
                Ok(pixels) => pixels,
                Err(direct) => {
                    let full = self.grab(0, 0, self.root_width, self.root_height).map_err(|full| {
                        anyhow!(
                            "{full}\n(the direct capture of the monitor rectangle failed too: {direct})"
                        )
                    })?;
                    crop(&full, self.root_width, x, y, width, height).with_context(|| {
                        format!(
                            "cropping monitor {index} out of the full-screen capture \
                             ({width}x{height} at {x},{y})"
                        )
                    })?
                }
            };

            let frame = encode_jpeg(
                &rgba,
                width,
                height,
                max_width,
                quality,
                jpeg_encoder::ColorType::Rgba,
            )?;
            Ok((frame, Geometry { x, y, width, height }))
        }

        /// Intersect a monitor rectangle with the root window.
        ///
        /// Guards against reporting geometries that cannot be captured: monitors
        /// at negative offsets, logical rectangles wider than the root under
        /// display scaling, and monitor lists left over from a mode change.
        fn fit_to_root(&self, monitor: &MonitorInfo) -> Result<(i32, i32, u32, u32)> {
            fit_rect(monitor, self.root_width, self.root_height)
        }

        /// One `GetImage` against the root window, converted to RGBA.
        ///
        /// The plane mask is tried twice: everything, then only the bits the root
        /// depth can hold. Deep-colour (30-bit) servers reject a mask that reaches
        /// past the drawable's depth, and that single retry is the difference
        /// between "screen capture failed" and a working capture on such setups.
        fn grab(&self, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
            let depth_mask: u32 = match self.root_depth {
                32 | 0 => !0,
                d => (1u32 << d) - 1,
            };
            let mut last_error = None;
            for mask in [(!0u32), depth_mask] {
                let attempt = match self.conn.get_image(
                    ImageFormat::Z_PIXMAP,
                    self.root,
                    x as i16,
                    y as i16,
                    width as u16,
                    height as u16,
                    mask,
                ) {
                    Ok(cookie) => cookie.reply().map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                };
                match attempt {
                    Ok(reply) => {
                        return to_rgba(
                            &reply.data,
                            width,
                            height,
                            self.bits_per_pixel,
                            self.lsb_first,
                        )
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            Err(anyhow!(
                "the X server refused GetImage for {width}x{height} at {x},{y} (root {}x{}, \
                 depth {}): {}. Common causes: the screen is locked or the display is \
                 asleep (DPMS), the request falls outside the root window, or this is a \
                 Wayland session that does not expose the root pixmap",
                self.root_width,
                self.root_height,
                self.root_depth,
                last_error.unwrap_or_default()
            ))
        }
    }

/// Intersect a monitor rectangle with the root window, as a free function so
    /// the maths can be tested without an X server.
    fn fit_rect(monitor: &MonitorInfo, root_width: u32, root_height: u32) -> Result<(i32, i32, u32, u32)> {
        let max_w = root_width as i32;
        let max_h = root_height as i32;
        let x0 = monitor.x.clamp(0, max_w);
        let y0 = monitor.y.clamp(0, max_h);
        let x1 = monitor.x.saturating_add(monitor.width as i32).clamp(0, max_w);
        let y1 = monitor.y.saturating_add(monitor.height as i32).clamp(0, max_h);
        let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if w == 0 || h == 0 {
            bail!(
                "monitor {} '{}' sits at {}x{}+{},{} which is outside the root window \
                 ({}x{}); RandR and the root disagree -- this usually means display \
                 scaling or a stale monitor layout. Run `reaven screens` to see what \
                 the agent reports",
                monitor.index, monitor.name, monitor.width, monitor.height,
                monitor.x, monitor.y, root_width, root_height
            );
        }
        Ok((x0, y0, w.min(65535), h.min(65535)))
    }

    /// Cut a rectangle out of a tightly packed RGBA image.
    fn crop(src: &[u8], src_width: u32, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
        let stride = (src_width * 4) as usize;
        let row_bytes = (width * 4) as usize;
        let start = y as usize * stride + x as usize * 4;
        // The last row only needs `row_bytes`, not a whole stride: a crop that
        // starts mid-row ends before the end of its final stride.
        let end = start + (height as usize - 1) * stride + row_bytes;
        if width == 0 || height == 0 || end > src.len() {
            bail!(
                "cannot crop {width}x{height} at {x},{y} out of a {}x{} image",
                src_width,
                src.len() / stride.max(1)
            );
        }
        let mut out = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let o = start + row * stride;
            out.extend_from_slice(&src[o..o + row_bytes]);
        }
        Ok(out)
    }

    /// Convert an X11 ZPixmap into tightly packed RGBA.
    ///
    /// X gives us a packed pixel buffer whose channel order depends on
    /// bits-per-pixel and the server's byte order, plus an implicit row stride
    /// that may include padding -- hence deriving stride from the payload size.
    fn to_rgba(data: &[u8], width: u32, height: u32, bpp: u8, lsb_first: bool) -> Result<Vec<u8>> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 {
            bail!("captured an empty image");
        }
        if data.len() < h {
            bail!("X server returned a truncated image ({} bytes)", data.len());
        }
        let stride = data.len() / h;
        let bytes_per_pixel = (bpp as usize).div_ceil(8);
        if stride < w * bytes_per_pixel {
            bail!("unexpected image stride: {stride} bytes for {w}px at {bpp}bpp");
        }

        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let row = &data[y * stride..];
            let out_row = &mut out[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let o = x * bytes_per_pixel;
                let p = &row[o..o + bytes_per_pixel];
                let (r, g, b) = match (bpp, lsb_first) {
                    (32, true) => (p[2], p[1], p[0]),
                    (32, false) => (p[1], p[2], p[3]),
                    (24, true) => (p[2], p[1], p[0]),
                    (24, false) => (p[0], p[1], p[2]),
                    (16, true) | (16, false) => {
                        let raw = if lsb_first {
                            u16::from_le_bytes([p[0], p[1]])
                        } else {
                            u16::from_be_bytes([p[0], p[1]])
                        };
                        let scale = |v: u16, max: u16| ((v as u32 * 255) / max as u32) as u8;
                        (
                            scale((raw >> 11) & 0x1f, 0x1f),
                            scale((raw >> 5) & 0x3f, 0x3f),
                            scale(raw & 0x1f, 0x1f),
                        )
                    }
                    _ => bail!("unsupported X11 pixel format: {bpp} bits per pixel"),
                };
                let q = x * 4;
                out_row[q] = r;
                out_row[q + 1] = g;
                out_row[q + 2] = b;
                out_row[q + 3] = 255;
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::to_rgba;

        #[test]
        fn converts_32bpp_bgra_little_endian() {
            // Two pixels: pure red (0x00FF0000) and pure green, little-endian BGRX.
            let data = vec![0, 0, 255, 0, 0, 255, 0, 0];
            let rgba = to_rgba(&data, 2, 1, 32, true).unwrap();
            assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
            assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);
        }

        #[test]
        fn converts_24bpp_and_copes_with_row_padding() {
            // 2px wide, 2 rows, 3 bytes/px + 2 bytes padding per row.
            // Little-endian BGR: red is 00 00 FF, blue is FF 00 00.
            let mut data = Vec::new();
            for _ in 0..2 {
                data.extend_from_slice(&[0, 0, 255, 255, 0, 0]); // red, blue
                data.extend_from_slice(&[0, 0]); // row padding
            }
            let rgba = to_rgba(&data, 2, 2, 24, true).unwrap();
            assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
            assert_eq!(&rgba[4..8], &[0, 0, 255, 255]);
            assert_eq!(&rgba[8..12], &[255, 0, 0, 255]);
        }

        #[test]
        fn rejects_short_buffers() {
            assert!(to_rgba(&[0, 0, 0], 4, 4, 32, true).is_err());
        }

        fn monitor(x: i32, y: i32, width: u32, height: u32) -> super::super::MonitorInfo {
            super::super::MonitorInfo {
                index: 0,
                name: "test".into(),
                x,
                y,
                width,
                height,
                scale: 1.0,
                primary: true,
            }
        }

        #[test]
        fn a_monitor_inside_the_root_is_captured_unchanged() {
            let (x, y, w, h) = super::fit_rect(&monitor(0, 0, 1920, 1080), 1920, 1080).unwrap();
            assert_eq!((x, y, w, h), (0, 0, 1920, 1080));
        }

        #[test]
        fn a_second_monitor_is_offset_correctly() {
            let (x, y, w, h) = super::fit_rect(&monitor(1920, 0, 1920, 1080), 3840, 1080).unwrap();
            assert_eq!((x, y, w, h), (1920, 0, 1920, 1080));
        }

        #[test]
        fn rectangles_larger_than_the_root_are_clamped() {
            // e.g. fractional scaling: RandR reports a logical size the root cannot hold
            let (x, y, w, h) = super::fit_rect(&monitor(0, 0, 3000, 2000), 1920, 1080).unwrap();
            assert_eq!((x, y, w, h), (0, 0, 1920, 1080));
        }

        #[test]
        fn negative_origins_are_clamped() {
            let (x, y, w, h) = super::fit_rect(&monitor(-100, -50, 1920, 1080), 1920, 1080).unwrap();
            assert_eq!((x, y, w, h), (0, 0, 1820, 1030));
        }

        #[test]
        fn a_monitor_entirely_offscreen_is_reported_not_captured() {
            let err = super::fit_rect(&monitor(4000, 0, 1920, 1080), 1920, 1080).unwrap_err();
            assert!(format!("{err}").contains("outside the root window"));
        }

        #[test]
        fn crop_extracts_the_requested_rectangle() {
            // 3x2 image, one distinct byte value per pixel (RGBA), crop the right column
            let src: Vec<u8> = (0..6u8).flat_map(|i| [i, i, i, 255]).collect();
            let out = super::crop(&src, 3, 2, 0, 1, 2).unwrap();
            assert_eq!(out, vec![2, 2, 2, 255, 5, 5, 5, 255]);
        }

        #[test]
        fn crop_rejects_a_rectangle_outside_the_image() {
            let src = vec![0u8; 3 * 2 * 4];
            assert!(super::crop(&src, 3, 2, 0, 2, 2).is_err());
        }
    }
}

/// Check every monitor once, at agent start-up, so a capture problem is reported
/// when it happens rather than when someone first tries to watch the screen.
#[cfg(feature = "screen")]
pub fn self_test() -> Result<String> {
    let screen = x11::open()?;
    let mut report = String::new();
    for monitor in &screen.monitors {
        let status = match screen.capture(monitor.index, 64, 40) {
            Ok((frame, geometry)) => format!(
                "capture ok ({}x{} frame, {} bytes JPEG)",
                geometry.width,
                geometry.height,
                frame.jpeg.len()
            ),
            Err(e) => format!("capture FAILED: {e:#}"),
        };
        report.push_str(&format!(
            "  monitor {} '{}' {}x{} at {}+{} -- {}\n",
            monitor.index, monitor.name, monitor.width, monitor.height, monitor.x, monitor.y, status
        ));
    }
    Ok(report)
}

#[cfg(not(feature = "screen"))]
pub fn self_test() -> Result<String> {
    Ok(String::from("  (no screen support in this build)\n"))
}

#[cfg(feature = "screen")]
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    Ok(x11::open()?.monitors)
}

#[cfg(feature = "screen")]
pub fn capture_screen(
    monitor: u32,
    max_width: u32,
    quality: u8,
) -> Result<(EncodedFrame, Geometry)> {
    x11::open()?.capture(monitor, max_width, quality)
}

/// Stream a monitor until `stop` is set or `emit` returns `false`.
#[cfg(feature = "screen")]
pub fn run_screen_stream(
    monitor: u32,
    max_width: u32,
    quality: u8,
    fps: u8,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut emit: impl FnMut(EncodedFrame, Geometry) -> bool,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let screen = x11::open()?;
    // Fail fast on a bad index instead of streaming nothing.
    if screen.monitors.get(monitor as usize).is_none() {
        bail!(
            "monitor {monitor} does not exist; this display has {} monitor(s)",
            screen.monitors.len()
        );
    }
    let interval = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
    let mut next = Instant::now();
    let mut last_error: Option<(String, Instant)> = None;
    while !stop.load(Ordering::Relaxed) {
        match screen.capture(monitor, max_width, quality) {
            Ok((frame, geometry)) => {
                if !emit(frame, geometry) {
                    break;
                }
            }
            // A transient failure (screen locked, mode switch) should not kill a
            // live session: report it and keep trying. Repeated identical failures
            // are collapsed so a locked screen does not flood the console at fps.
            Err(e) => {
                let message = format!("{e:#}");
                let now = Instant::now();
                let dominated = matches!(
                    &last_error,
                    Some((previous, at)) if previous == &message && now.duration_since(*at) < Duration::from_secs(10)
                );
                if !dominated {
                    eprintln!("reaven: screen capture failed: {message}");
                    last_error = Some((message, now));
                }
            }
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // fell behind: keep the cadence, don't try to catch up
        }
    }
    Ok(())
}

#[cfg(not(feature = "screen"))]
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    bail!("this build has no screen support (rebuild with --features screen)")
}

#[cfg(not(feature = "screen"))]
pub fn capture_screen(_: u32, _: u32, _: u8) -> Result<(EncodedFrame, Geometry)> {
    bail!("this build has no screen support (rebuild with --features screen)")
}

#[cfg(not(feature = "screen"))]
pub fn run_screen_stream(
    _: u32,
    _: u32,
    _: u8,
    _: u8,
    _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _: impl FnMut(EncodedFrame, Geometry) -> bool,
) -> Result<()> {
    bail!("this build has no screen support (rebuild with --features screen)")
}

// ------------------------------------------------------------------ webcams ---

#[cfg(feature = "webcam")]
fn query_cameras() -> Result<Vec<nokhwa::utils::CameraInfo>> {
    use nokhwa::utils::ApiBackend;
    nokhwa::nokhwa_initialize(|_| {});
    let cams = nokhwa::query(ApiBackend::Auto).context("enumerating webcams")?;
    Ok(cams)
}

#[cfg(feature = "webcam")]
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    let cams = query_cameras()?;
    Ok(cams
        .into_iter()
        .enumerate()
        .map(|(index, c)| CameraInfo {
            index: index as u32,
            name: c.human_name(),
        })
        .collect())
}

#[cfg(feature = "webcam")]
pub fn open_camera(index: u32, width: u32) -> Result<nokhwa::Camera> {
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{RequestedFormat, RequestedFormatType};

    let cams = query_cameras()?;
    if cams.is_empty() {
        bail!("no webcams detected");
    }
    let target = cams
        .get(index as usize)
        .ok_or_else(|| anyhow!("camera {index} does not exist; run `reaven cameras` to list"))?;

    // Prefer an RGB format at a sane size; fall back to whatever the device offers.
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = nokhwa::Camera::new(target.index().clone(), requested)
        .with_context(|| format!("opening webcam '{}'", target.human_name()))?;
    camera.open_stream().context("starting webcam stream")?;
    let _ = width; // mode is chosen by the device; honoured where supported
    Ok(camera)
}

#[cfg(feature = "webcam")]
fn camera_frame_jpeg(
    camera: &mut nokhwa::Camera,
    max_width: u32,
    quality: u8,
) -> Result<EncodedFrame> {
    use nokhwa::pixel_format::RgbFormat;

    let frame = camera.frame().context("reading webcam frame")?;
    let image = frame
        .decode_image::<RgbFormat>()
        .context("decoding webcam frame")?;
    encode_jpeg(
        image.as_raw(),
        image.width(),
        image.height(),
        max_width,
        quality,
        jpeg_encoder::ColorType::Rgb,
    )
}

#[cfg(feature = "webcam")]
pub fn capture_camera(index: u32, width: u32, quality: u8) -> Result<EncodedFrame> {
    let mut camera = open_camera(index, width)?;
    // First frames from a cold sensor are often dark/garbage; discard a couple.
    for _ in 0..2 {
        let _ = camera.frame();
    }
    let out = camera_frame_jpeg(&mut camera, width, quality);
    let _ = camera.stop_stream();
    out
}

/// Stream a webcam until `stop` is set or `emit` returns `false`.
#[cfg(feature = "webcam")]
pub fn run_camera_stream(
    index: u32,
    max_width: u32,
    quality: u8,
    fps: u8,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut emit: impl FnMut(EncodedFrame) -> bool,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let mut camera = open_camera(index, max_width)?;
    let interval = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match camera_frame_jpeg(&mut camera, max_width, quality) {
            Ok(frame) => {
                if !emit(frame) {
                    break;
                }
            }
            Err(e) => eprintln!("reaven: webcam frame failed: {e}"),
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    let _ = camera.stop_stream();
    Ok(())
}

#[cfg(not(feature = "webcam"))]
pub fn list_cameras() -> Result<Vec<CameraInfo>> {
    bail!("this build has no webcam support (rebuild with --features webcam)")
}

#[cfg(not(feature = "webcam"))]
pub fn capture_camera(_: u32, _: u32, _: u8) -> Result<EncodedFrame> {
    bail!("this build has no webcam support (rebuild with --features webcam)")
}

#[cfg(not(feature = "webcam"))]
pub fn run_camera_stream(
    _: u32,
    _: u32,
    _: u8,
    _: u8,
    _: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _: impl FnMut(EncodedFrame) -> bool,
) -> Result<()> {
    bail!("this build has no webcam support (rebuild with --features webcam)")
}

// ------------------------------------------------------------------- input ---

/// Map normalised coordinates onto absolute desktop pixels for the stream that
/// produced them. Clamped so a stale or malicious client cannot park the
/// pointer outside the captured monitor.
#[cfg(feature = "input")]
fn absolute(geometry: Geometry, nx: f32, ny: f32) -> (i32, i32) {
    let nx = if nx.is_finite() {
        nx.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let ny = if ny.is_finite() {
        ny.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let w = geometry.width.saturating_sub(1) as f32;
    let h = geometry.height.saturating_sub(1) as f32;
    (
        geometry.x + (nx * w).round() as i32,
        geometry.y + (ny * h).round() as i32,
    )
}

#[cfg(feature = "input")]
fn fkey(n: u8) -> Option<enigo::Key> {
    use enigo::Key;
    Some(match n {
        1 => Key::F1,
        2 => Key::F2,
        3 => Key::F3,
        4 => Key::F4,
        5 => Key::F5,
        6 => Key::F6,
        7 => Key::F7,
        8 => Key::F8,
        9 => Key::F9,
        10 => Key::F10,
        11 => Key::F11,
        12 => Key::F12,
        13 => Key::F13,
        14 => Key::F14,
        15 => Key::F15,
        16 => Key::F16,
        17 => Key::F17,
        18 => Key::F18,
        19 => Key::F19,
        20 => Key::F20,
        21 => Key::F21,
        22 => Key::F22,
        23 => Key::F23,
        24 => Key::F24,
        _ => return None,
    })
}

#[cfg(feature = "input")]
fn key_from_name(name: &str) -> Result<enigo::Key> {
    use enigo::Key;
    let lower = name.trim().to_ascii_lowercase();
    let key = match lower.as_str() {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" => Key::Alt,
        "meta" | "super" | "win" | "cmd" | "command" => Key::Meta,
        "altgr" => Key::Alt,
        "return" | "enter" => Key::Return,
        "tab" => Key::Tab,
        "escape" | "esc" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "space" => Key::Space,
        "capslock" => Key::CapsLock,
        "up" | "arrowup" => Key::UpArrow,
        "down" | "arrowdown" => Key::DownArrow,
        "left" | "arrowleft" => Key::LeftArrow,
        "right" | "arrowright" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        other if other.starts_with('f') && other.len() <= 3 => {
            let n: u8 = other[1..]
                .parse()
                .map_err(|_| anyhow!("unknown key '{name}'"))?;
            fkey(n).ok_or_else(|| anyhow!("unknown function key '{name}'"))?
        }
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Unicode(c),
                _ => bail!("unknown key '{name}'"),
            }
        }
    };
    Ok(key)
}

#[cfg(feature = "input")]
fn mouse_button(button: crate::protocol::MouseButton) -> enigo::Button {
    use crate::protocol::MouseButton;
    match button {
        MouseButton::Left => enigo::Button::Left,
        MouseButton::Middle => enigo::Button::Middle,
        MouseButton::Right => enigo::Button::Right,
    }
}

/// Apply a batch of input events. Returns how many were performed.
#[cfg(feature = "input")]
pub fn apply_input(events: &[InputEvent], geometry: Geometry) -> Result<usize> {
    use enigo::{Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
    use std::time::Duration;

    if events.is_empty() {
        return Ok(0);
    }
    if geometry.width == 0 || geometry.height == 0 {
        bail!("no active screen stream to map coordinates onto");
    }
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| {
        anyhow!("input injection unavailable ({e}); on Wayland this needs a session with the desktop portal")
    })?;
    let mut performed = 0usize;

    for event in events {
        match event {
            InputEvent::Move { x, y } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
            }
            InputEvent::Down { button, x, y } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
                enigo.button(mouse_button(*button), Direction::Press)?;
            }
            InputEvent::Up { button, x, y } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
                enigo.button(mouse_button(*button), Direction::Release)?;
            }
            InputEvent::Click { button, x, y } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
                enigo.button(mouse_button(*button), Direction::Click)?;
            }
            InputEvent::DoubleClick { button, x, y } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
                enigo.button(mouse_button(*button), Direction::Click)?;
                std::thread::sleep(Duration::from_millis(40));
                enigo.button(mouse_button(*button), Direction::Click)?;
            }
            InputEvent::Scroll { x, y, dx, dy } => {
                let (ax, ay) = absolute(geometry, *x, *y);
                enigo.move_mouse(ax, ay, Coordinate::Abs)?;
                if *dy != 0 {
                    enigo.scroll(*dy, enigo::Axis::Vertical)?;
                }
                if *dx != 0 {
                    enigo.scroll(*dx, enigo::Axis::Horizontal)?;
                }
            }
            InputEvent::Key { key, pressed } => {
                let key = key_from_name(key)?;
                enigo.key(
                    key,
                    if *pressed {
                        Direction::Press
                    } else {
                        Direction::Release
                    },
                )?;
            }
            InputEvent::Text { text } => {
                enigo.text(text)?;
            }
            InputEvent::Combo { keys } => {
                if keys.is_empty() {
                    continue;
                }
                let resolved: Vec<enigo::Key> = keys
                    .iter()
                    .map(|k| key_from_name(k))
                    .collect::<Result<_>>()?;
                for key in &resolved {
                    enigo.key(*key, Direction::Press)?;
                }
                for key in resolved.iter().rev() {
                    enigo.key(*key, Direction::Release)?;
                }
            }
        }
        performed += 1;
    }
    Ok(performed)
}

#[cfg(not(feature = "input"))]
pub fn apply_input(_: &[InputEvent], _: Geometry) -> Result<usize> {
    bail!("this build has no input support (rebuild with --features input)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalised_coordinates_map_to_pixels() {
        #[cfg(feature = "input")]
        {
            let g = Geometry {
                x: 100,
                y: 50,
                width: 1000,
                height: 500,
            };
            assert_eq!(absolute(g, 0.0, 0.0), (100, 50));
            assert_eq!(absolute(g, 1.0, 1.0), (1099, 549));
            assert_eq!(absolute(g, 0.5, 0.5), (600, 300));
            // Out-of-range and NaN inputs are clamped per axis, not propagated.
            assert_eq!(absolute(g, 2.5, -1.0), (1099, 50));
            assert_eq!(absolute(g, -1.0, 2.5), (100, 549));
            assert_eq!(absolute(g, f32::NAN, 0.5), (100, 300));
        }
    }

    #[test]
    fn rejects_unknown_keys() {
        #[cfg(feature = "input")]
        {
            assert!(key_from_name("notakey").is_err());
            assert!(key_from_name("a").is_ok());
            assert!(key_from_name("F5").is_ok());
            assert!(key_from_name("ctrl").is_ok());
        }
    }
}
