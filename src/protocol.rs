//! Wire protocol: newline-delimited JSON frames relayed between peers.
//!
//! Every frame is one JSON object followed by `\n`. The relay rewrites the
//! `from` field so a peer cannot spoof another peer's identity.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped when the frame set changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 2;

/// Upper bound on a single frame. A 4K JPEG at quality 90 base64-encodes to
/// roughly 2 MB, so this leaves plenty of headroom while still refusing
/// obviously hostile input.
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PeerKind {
    /// A machine being administered (`reaven connect`).
    #[default]
    Agent,
    /// A human/client connecting to drive an agent.
    Operator,
}

/// Advertised by a peer in `Hello` and repeated in `PeerList` entries so a
/// client can render a useful device picker.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    #[serde(default)]
    pub kind: PeerKind,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub version: String,
}

impl PeerInfo {
    pub fn local(name: &str, kind: PeerKind) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            hostname: hostname(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Which pixel source a video stream should read from.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoSource {
    /// A physical monitor. `monitor` indexes `reaven screens`.
    Screen {
        #[serde(default)]
        monitor: u32,
    },
    /// A webcam. `index` indexes `reaven cameras`.
    Camera {
        #[serde(default)]
        index: u32,
    },
}

impl Default for VideoSource {
    fn default() -> Self {
        VideoSource::Screen { monitor: 0 }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// One low-level input action. Coordinates are normalised to `0.0..=1.0`
/// across the most recently streamed frame, so the sending device never needs
/// to know the remote resolution -- important for a phone screen whose aspect
/// ratio will not match the desktop's.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum InputEvent {
    /// Absolute pointer move.
    Move { x: f32, y: f32 },
    /// Button pressed while positioned at (x, y).
    Down { button: MouseButton, x: f32, y: f32 },
    /// Button released at (x, y).
    Up { button: MouseButton, x: f32, y: f32 },
    /// Press + release in one action.
    Click { button: MouseButton, x: f32, y: f32 },
    /// Two quick clicks.
    DoubleClick { button: MouseButton, x: f32, y: f32 },
    /// Wheel/trackpad scroll at (x, y). Positive `dy` scrolls down.
    Scroll { x: f32, y: f32, dx: i32, dy: i32 },
    /// A single named key press/release, e.g. `Return`, `F5`, `a`.
    Key { key: String, pressed: bool },
    /// Type a string (layout-aware Unicode).
    Text { text: String },
    /// Chord such as `["ctrl", "alt", "t"]`, pressed in order and released in reverse.
    Combo { keys: Vec<String> },
}

impl MouseButton {
    pub fn parse(s: &str) -> Result<MouseButton> {
        match s.to_ascii_lowercase().as_str() {
            "left" | "l" | "1" => Ok(MouseButton::Left),
            "middle" | "m" | "2" => Ok(MouseButton::Middle),
            "right" | "r" | "3" => Ok(MouseButton::Right),
            other => bail!("unknown mouse button '{other}' (use left, middle or right)"),
        }
    }
}

/// Outcome of a `Request`.
///
/// Serialised *untagged*: a success is the payload object itself, a failure is
/// a bare JSON string. Serde's default `Result` encoding would wrap both in
/// `{"Ok": …}` / `{"Err": …}`, which every non-Rust client (the Android app,
/// the Python test harness) would have to unwrap for no benefit.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ResponseResult {
    Ok(Payload),
    Err(String),
}

impl ResponseResult {
    pub fn into_result(self) -> std::result::Result<Payload, String> {
        match self {
            ResponseResult::Ok(payload) => Ok(payload),
            ResponseResult::Err(message) => Err(message),
        }
    }
}

// `VideoFrame`/`VideoStop`/`VideoError` deliberately share the `Video` prefix:
// the names are the wire tags, which the Android client and docs depend on.
#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    // ----- session -----
    Hello {
        name: String,
        password: Option<String>,
        #[serde(default)]
        kind: PeerKind,
        #[serde(default)]
        info: Option<PeerInfo>,
    },
    Welcome,

    // ----- generic request/response (exec, files, snapshots) -----
    Request {
        id: String,
        from: String,
        target: String,
        op: Operation,
    },
    Response {
        id: String,
        from: String,
        to: String,
        result: ResponseResult,
    },

    // ----- discovery -----
    ListPeers {
        id: String,
        from: String,
    },
    PeerList {
        id: String,
        to: String,
        peers: Vec<PeerInfo>,
    },

    // ----- interactive shell -----
    ShellStart {
        id: String,
        from: String,
        target: String,
        rows: u16,
        cols: u16,
        term: String,
    },
    ShellInput {
        id: String,
        from: String,
        to: String,
        data: String,
    },
    ShellResize {
        id: String,
        from: String,
        to: String,
        rows: u16,
        cols: u16,
    },
    ShellClose {
        id: String,
        from: String,
        to: String,
    },
    ShellOutput {
        id: String,
        from: String,
        to: String,
        data: String,
    },
    ShellExit {
        id: String,
        from: String,
        to: String,
        code: i32,
    },

    // ----- screen / webcam streaming -----
    VideoStart {
        id: String,
        from: String,
        target: String,
        #[serde(default)]
        source: VideoSource,
        /// Down-scale so the long edge is at most this many pixels (0 = native).
        #[serde(default)]
        max_width: u32,
        /// JPEG quality, 1..=100.
        #[serde(default = "default_quality")]
        quality: u8,
        /// Target frames per second, 1..=60.
        #[serde(default = "default_fps")]
        fps: u8,
    },
    VideoStop {
        id: String,
        from: String,
        to: String,
    },
    VideoFrame {
        id: String,
        from: String,
        to: String,
        seq: u64,
        width: u32,
        height: u32,
        #[serde(default)]
        source: VideoSource,
        /// Base64 JPEG.
        data: String,
    },
    VideoError {
        id: String,
        from: String,
        to: String,
        message: String,
    },
    /// Fire-and-forget input injection; the agent replies with `Response` using the same id.
    Control {
        id: String,
        from: String,
        target: String,
        /// Stream whose geometry the normalised coordinates refer to.
        /// Omitted means "the newest active screen stream".
        #[serde(default)]
        stream: Option<String>,
        events: Vec<InputEvent>,
    },

    Error {
        message: String,
    },
}

fn default_quality() -> u8 {
    70
}

fn default_fps() -> u8 {
    12
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Exec {
        command: String,
    },
    Upload {
        path: String,
        data: String,
    },
    Download {
        path: String,
    },
    /// One-shot JPEG of a monitor.
    Screenshot {
        #[serde(default)]
        monitor: u32,
        #[serde(default = "default_quality")]
        quality: u8,
        #[serde(default)]
        max_width: u32,
    },
    /// One-shot JPEG of a webcam.
    CameraSnapshot {
        #[serde(default)]
        index: u32,
        #[serde(default)]
        width: u32,
        #[serde(default = "default_quality")]
        quality: u8,
    },
    /// Available monitors as JSON (`Json` payload).
    ListScreens,
    /// Available webcams as JSON (`Json` payload).
    ListCameras,
    /// Cheap liveness + build check.
    Ping,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Text {
        stdout: String,
        stderr: String,
        code: i32,
    },
    File {
        data: String,
    },
    /// Base64 image plus metadata so clients can size their surface before decode.
    Image {
        data: String,
        width: u32,
        height: u32,
        format: String,
    },
    Json {
        data: String,
    },
}

impl Payload {
    pub fn text(body: impl Into<String>) -> Payload {
        Payload::Text {
            stdout: body.into(),
            stderr: String::new(),
            code: 0,
        }
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    let mut line = serde_json::to_vec(frame)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Frame> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf).await?;
    if n == 0 {
        bail!("connection closed");
    }
    if buf.len() > MAX_FRAME_BYTES {
        bail!("protocol frame exceeded {MAX_FRAME_BYTES} bytes");
    }
    serde_json::from_slice(&buf).context("invalid protocol frame")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: &Frame) {
        let encoded = serde_json::to_string(frame).unwrap();
        let decoded: Frame = serde_json::from_str(&encoded).unwrap();
        assert_eq!(encoded, serde_json::to_string(&decoded).unwrap());
    }

    #[test]
    fn frames_roundtrip() {
        roundtrip(&Frame::Welcome);
        roundtrip(&Frame::Hello {
            name: "pc".into(),
            password: Some("pw".into()),
            kind: PeerKind::Agent,
            info: Some(PeerInfo::local("pc", PeerKind::Agent)),
        });
        roundtrip(&Frame::VideoStart {
            id: "s1".into(),
            from: "op".into(),
            target: "pc".into(),
            source: VideoSource::Screen { monitor: 1 },
            max_width: 1280,
            quality: 60,
            fps: 15,
        });
        roundtrip(&Frame::VideoFrame {
            id: "s1".into(),
            from: "pc".into(),
            to: "op".into(),
            seq: 7,
            width: 1920,
            height: 1080,
            source: VideoSource::Camera { index: 0 },
            data: "aGk=".into(),
        });
        roundtrip(&Frame::Control {
            id: "c1".into(),
            from: "op".into(),
            target: "pc".into(),
            stream: Some("s1".into()),
            events: vec![
                InputEvent::Move { x: 0.5, y: 0.25 },
                InputEvent::Click {
                    button: MouseButton::Left,
                    x: 0.5,
                    y: 0.25,
                },
                InputEvent::Scroll {
                    x: 0.5,
                    y: 0.5,
                    dx: 0,
                    dy: -2,
                },
                InputEvent::Key {
                    key: "Return".into(),
                    pressed: true,
                },
                InputEvent::Combo {
                    keys: vec!["ctrl".into(), "alt".into(), "t".into()],
                },
            ],
        });
    }

    /// Older clients omit `kind`/`info`; they must still parse.
    #[test]
    fn legacy_hello_parses() {
        let json = r#"{"type":"hello","name":"laptop","password":null}"#;
        let frame: Frame = serde_json::from_str(json).unwrap();
        match frame {
            Frame::Hello {
                kind, info, name, ..
            } => {
                assert_eq!(kind, PeerKind::Agent);
                assert!(info.is_none());
                assert_eq!(name, "laptop");
            }
            _ => panic!("wrong frame"),
        }
    }

    /// The wire shape the Android client and Python harness rely on: the
    /// payload object at `result` for success, a bare string for failure.
    #[test]
    fn response_result_is_untagged() {
        let ok = Frame::Response {
            id: "1".into(),
            from: "a".into(),
            to: "b".into(),
            result: ResponseResult::Ok(Payload::Image {
                data: "aGk=".into(),
                width: 10,
                height: 20,
                format: "jpeg".into(),
            }),
        };
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains(r#""result":{"kind":"image""#), "{json}");
        assert!(!json.contains(r#""Ok""#), "wrapped in Ok: {json}");
        let parsed: Frame = serde_json::from_str(&json).unwrap();
        match parsed {
            Frame::Response {
                result: ResponseResult::Ok(Payload::Image { width, .. }),
                ..
            } => {
                assert_eq!(width, 10);
            }
            other => panic!("wrong round-trip: {other:?}"),
        }

        let err = Frame::Response {
            id: "1".into(),
            from: "a".into(),
            to: "b".into(),
            result: ResponseResult::Err("'pc' is offline".into()),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains(r#""result":"'pc' is offline""#), "{json}");
    }

    #[test]
    fn frame_tag_is_snake_case() {
        let json = serde_json::to_string(&Frame::ListPeers {
            id: "1".into(),
            from: "op".into(),
        })
        .unwrap();
        assert!(json.contains("\"type\":\"list_peers\""), "{json}");
        let json = serde_json::to_string(&Operation::ListCameras).unwrap();
        assert!(json.contains("\"op\":\"list_cameras\""), "{json}");
    }

    #[test]
    fn input_event_tags() {
        let json = serde_json::to_string(&InputEvent::DoubleClick {
            button: MouseButton::Right,
            x: 0.1,
            y: 0.2,
        })
        .unwrap();
        assert!(json.contains("\"event\":\"double_click\""), "{json}");
        assert!(json.contains("\"button\":\"right\""), "{json}");
    }
}
