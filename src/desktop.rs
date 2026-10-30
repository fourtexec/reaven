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

#[cfg(feature = "screen")]
fn monitors() -> Result<Vec<xcap::Monitor>> {
    xcap::Monitor::all().context("enumerating monitors")
}

#[cfg(feature = "screen")]
fn monitor_at(index: u32) -> Result<xcap::Monitor> {
    let monitors = monitors()?;
    if monitors.is_empty() {
        bail!("no monitors detected (is DISPLAY or XDG_SESSION_TYPE set?)");
    }
    monitors
        .into_iter()
        .nth(index as usize)
        .ok_or_else(|| anyhow!("monitor {index} does not exist; run `reaven screens` to list"))
}

#[cfg(feature = "screen")]
pub fn list_monitors() -> Result<Vec<MonitorInfo>> {
    let all = monitors()?;
    let mut out = Vec::with_capacity(all.len());
    for (index, m) in all.iter().enumerate() {
        let width = m.width().unwrap_or(0);
        let height = m.height().unwrap_or(0);
        let scale = m.scale_factor().unwrap_or(1.0);
        out.push(MonitorInfo {
            index: index as u32,
            name: m.name().unwrap_or_else(|_| format!("display {index}")),
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            width,
            height,
            scale,
            primary: index == 0,
        });
    }
    Ok(out)
}

#[cfg(feature = "screen")]
pub fn capture_screen(
    monitor: u32,
    max_width: u32,
    quality: u8,
) -> Result<(EncodedFrame, Geometry)> {
    let m = monitor_at(monitor)?;
    let geometry = Geometry {
        x: m.x().unwrap_or(0),
        y: m.y().unwrap_or(0),
        width: m.width().unwrap_or(0),
        height: m.height().unwrap_or(0),
    };
    let image = m.capture_image().context("capturing monitor")?;
    let frame = encode_jpeg(
        image.as_raw(),
        image.width(),
        image.height(),
        max_width,
        quality,
        jpeg_encoder::ColorType::Rgba,
    )?;
    Ok((frame, geometry))
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

    let m = monitor_at(monitor)?;
    let geometry = Geometry {
        x: m.x().unwrap_or(0),
        y: m.y().unwrap_or(0),
        width: m.width().unwrap_or(0),
        height: m.height().unwrap_or(0),
    };
    let interval = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        match m.capture_image() {
            Ok(image) => {
                let encoded = encode_jpeg(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    max_width,
                    quality,
                    jpeg_encoder::ColorType::Rgba,
                )?;
                if !emit(encoded, geometry) {
                    break;
                }
            }
            // A transient capture failure (screen locked, resize) should not
            // kill a session; keep trying until the deadline.
            Err(e) => eprintln!("reaven: screen capture failed: {e}"),
        }
        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // fell behind: keep the cadence, don't try to catch up
        }
        // Guard against a pathological capture path pinning a core.
        if started.elapsed() < Duration::from_millis(1) {
            std::thread::sleep(Duration::from_millis(1));
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
