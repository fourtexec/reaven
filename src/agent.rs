//! The agent: the process that runs on a machine you want to reach.
//!
//! Responsibilities:
//!   * answer `Request` frames (exec, file transfer, snapshots, enumeration)
//!   * serve interactive PTY shells
//!   * stream screen / webcam frames on demand
//!   * inject mouse/keyboard input when asked
//!
//! Every remote-control capability is announced on the agent's own console so
//! the person sitting at that machine has a local record of it.

use std::collections::HashMap;
use std::io::Read as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex as StdMutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::desktop::{self, Geometry};
use crate::protocol::{
    Frame, InputEvent, Operation, Payload, PeerInfo, PeerKind, ResponseResult, VideoSource,
};
use crate::transport;

#[derive(Debug, Clone, Copy)]
pub struct AgentOptions {
    /// Whether remote input injection is permitted. Enabled by default; pass
    /// `--no-input` for a view-only agent.
    pub allow_input: bool,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self { allow_input: true }
    }
}

enum ShellControl {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}

type ShellMap = Arc<StdMutex<HashMap<String, std_mpsc::Sender<ShellControl>>>>;

struct StreamCtx {
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    source: VideoSource,
}

type StreamMap = Arc<StdMutex<HashMap<String, StreamCtx>>>;

/// Geometry of the most recent screen stream, used to map normalised pointer
/// coordinates onto absolute desktop pixels.
type LastGeometry = Arc<StdMutex<Option<(String, Geometry)>>>;

pub async fn run_peer(
    server: &str,
    name: &str,
    password: Option<String>,
    insecure: bool,
    options: AgentOptions,
) -> Result<()> {
    // A transient relay/proxy/network timeout must not permanently take the
    // agent offline. Retry transport failures with capped exponential backoff;
    // authentication and protocol errors still return to the caller.
    let mut retry_seconds = 1_u64;
    loop {
        match run_peer_session(server, name, password.clone(), insecure, options).await {
            Ok(()) => return Ok(()),
            Err(error) if is_transient_transport_error(&error) => {
                eprintln!(
                    "relay connection lost ({error:#}); reconnecting in {retry_seconds}s"
                );
                tokio::time::sleep(Duration::from_secs(retry_seconds)).await;
                retry_seconds = (retry_seconds * 2).min(30);
            }
            Err(error) => return Err(error),
        }
    }
}

fn is_transient_transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(io_error) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                io_error.kind(),
                std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::Interrupted
                    | std::io::ErrorKind::WouldBlock
            ) {
                return true;
            }
        }
        let message = cause.to_string().to_ascii_lowercase();
        message.contains("connection closed")
            || message.contains("timed out")
            || message.contains("timeout")
            || message.contains("deadline has elapsed")
            || message.contains("temporary failure in name resolution")
            || message.contains("network is unreachable")
    })
}

async fn run_peer_session(
    server: &str,
    name: &str,
    password: Option<String>,
    insecure: bool,
    options: AgentOptions,
) -> Result<()> {
    let ws = transport::connect(server, insecure).await?;
    let (mut reader, mut write) = ws.split();

    let info = PeerInfo::local(name, PeerKind::Agent);
    write
        .send(&Frame::Hello {
            name: name.to_owned(),
            password,
            kind: PeerKind::Agent,
            info: Some(info),
        })
        .await?;
    match reader.recv().await? {
        Frame::Welcome => println!("Reaven agent '{name}' online at {server} (WSS)"),
        Frame::Error { message } => bail!("server rejected connection: {message}"),
        _ => bail!("unexpected server response"),
    }
    if options.allow_input {
        println!("Remote input injection is ENABLED (mouse + keyboard). Use --no-input for view-only mode.");
    } else {
        println!("Remote input injection is disabled (view-only).");
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    let shells: ShellMap = Arc::new(StdMutex::new(HashMap::new()));
    let streams: StreamMap = Arc::new(StdMutex::new(HashMap::new()));
    let last_geometry: LastGeometry = Arc::new(StdMutex::new(None));

    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write.send(&frame).await.is_err() {
                break;
            }
        }
    });

    let result: Result<()> = async {
        loop {
            match reader.recv().await? {
                Frame::Request { id, from, op, .. } => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        // Snapshots and exec are blocking; keep the read loop free.
                        let result =
                            match tokio::task::spawn_blocking(move || execute_blocking(op)).await {
                                Ok(Ok(payload)) => ResponseResult::Ok(payload),
                                Ok(Err(message)) => ResponseResult::Err(message),
                                Err(e) => ResponseResult::Err(format!("agent task failed: {e}")),
                            };
                        let _ = tx.send(Frame::Response {
                            id,
                            from: String::new(),
                            to: from,
                            result,
                        });
                    });
                }
                Frame::ShellStart {
                    id,
                    from,
                    rows,
                    cols,
                    term,
                    ..
                } => {
                    if let Err(message) = start_peer_shell(
                        id.clone(),
                        from.clone(),
                        rows,
                        cols,
                        &term,
                        tx.clone(),
                        shells.clone(),
                    ) {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: String::new(),
                            to: from,
                            result: ResponseResult::Err(message),
                        });
                    }
                }
                Frame::ShellInput { id, data, .. } => {
                    if let Ok(bytes) = B64.decode(data) {
                        if let Some(sender) = shells.lock().unwrap().get(&id).cloned() {
                            let _ = sender.send(ShellControl::Input(bytes));
                        }
                    }
                }
                Frame::ShellResize { id, rows, cols, .. } => {
                    if let Some(sender) = shells.lock().unwrap().get(&id).cloned() {
                        let _ = sender.send(ShellControl::Resize(rows, cols));
                    }
                }
                Frame::ShellClose { id, .. } => {
                    if let Some(sender) = shells.lock().unwrap().get(&id).cloned() {
                        let _ = sender.send(ShellControl::Close);
                    }
                }

                // ---- video streaming ----
                Frame::VideoStart {
                    id,
                    from,
                    source,
                    max_width,
                    quality,
                    fps,
                    ..
                } => {
                    start_stream(
                        id,
                        from,
                        source,
                        max_width,
                        quality,
                        fps,
                        tx.clone(),
                        streams.clone(),
                        last_geometry.clone(),
                    );
                }
                Frame::VideoStop { id, from, .. } => {
                    let ctx = streams.lock().unwrap().remove(&id);
                    match ctx {
                        Some(ctx) => {
                            ctx.stop.store(true, Ordering::Relaxed);
                            println!("remote stream {id} stopped by '{from}'");
                        }
                        None => {
                            let _ = tx.send(Frame::VideoError {
                                id,
                                from: String::new(),
                                to: from,
                                message: "no such stream".into(),
                            });
                        }
                    }
                }

                // ---- remote input ----
                Frame::Control {
                    id,
                    from,
                    stream,
                    events,
                    ..
                } => {
                    let reply_to = from.clone();
                    let tx = tx.clone();
                    if !options.allow_input {
                        println!(
                            "REJECTED {n} input event(s) from '{from}': agent is view-only",
                            n = events.len()
                        );
                        let _ = tx.send(Frame::Response {
                            id,
                            from: String::new(),
                            to: reply_to,
                            result: ResponseResult::Err(
                                "this agent is running with --no-input (view-only)".into(),
                            ),
                        });
                        continue;
                    }
                    let geometry = {
                        let last = last_geometry.lock().unwrap().clone();
                        match stream {
                            Some(want) => match &last {
                                Some((id, geom)) if *id == want => Some(*geom),
                                _ => last.map(|(_, geom)| geom),
                            },
                            None => last.map(|(_, geom)| geom),
                        }
                    };
                    let Some(geometry) = geometry else {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: String::new(),
                            to: reply_to,
                            result: ResponseResult::Err(
                                "start a screen stream before sending input".into(),
                            ),
                        });
                        continue;
                    };
                    tokio::spawn(async move {
                        let result =
                            tokio::task::spawn_blocking(move || apply_events(&events, geometry))
                                .await;
                        let result = match result {
                            Ok(Ok(count)) => ResponseResult::Ok(Payload::text(format!(
                                "{count} event(s) applied"
                            ))),
                            Ok(Err(e)) => ResponseResult::Err(format!("{e:#}")),
                            Err(e) => ResponseResult::Err(format!("input task failed: {e}")),
                        };
                        let _ = tx.send(Frame::Response {
                            id,
                            from: String::new(),
                            to: reply_to,
                            result,
                        });
                    });
                }

                Frame::Error { message } => eprintln!("Reaven: {message}"),
                other => bail!("unexpected server frame: {other:?}"),
            }
        }
    }
    .await;

    // Tear down session-owned work before reconnecting. Otherwise a transient
    // socket failure could leave a PTY shell or capture thread behind.
    for (_, ctx) in streams.lock().unwrap().drain() {
        ctx.stop.store(true, Ordering::Relaxed);
    }
    for (_, sender) in shells.lock().unwrap().drain() {
        let _ = sender.send(ShellControl::Close);
    }
    writer.abort();
    result
}

fn apply_events(events: &[InputEvent], geometry: Geometry) -> Result<usize> {
    desktop::apply_input(events, geometry)
}

#[allow(clippy::too_many_arguments)]
fn start_stream(
    id: String,
    requester: String,
    source: VideoSource,
    max_width: u32,
    quality: u8,
    fps: u8,
    outbound: mpsc::UnboundedSender<Frame>,
    streams: StreamMap,
    last_geometry: LastGeometry,
) {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut map = streams.lock().unwrap();
        if map.contains_key(&id) {
            let _ = outbound.send(Frame::VideoError {
                id,
                from: String::new(),
                to: requester,
                message: "stream id already in use".into(),
            });
            return;
        }
        map.insert(
            id.clone(),
            StreamCtx {
                stop: stop.clone(),
                source,
            },
        );
    }

    let label = match source {
        VideoSource::Screen { monitor } => format!("screen (monitor {monitor})"),
        VideoSource::Camera { index } => format!("webcam {index}"),
    };
    println!("remote stream {id} started by '{requester}': {label}, {fps} fps, q{quality}");

    let seq = Arc::new(AtomicU64::new(0));
    thread::spawn(move || {
        let emit_tx = outbound.clone();
        let emit_id = id.clone();
        let emit_to = requester.clone();
        let emit_seq = seq.clone();
        let geom_slot = last_geometry.clone();
        let geom_id = id.clone();

        let emit = move |frame: desktop::EncodedFrame, geom: Geometry| -> bool {
            if let Ok(mut slot) = geom_slot.lock() {
                *slot = Some((geom_id.clone(), geom));
            }
            let n = emit_seq.fetch_add(1, Ordering::Relaxed);
            emit_tx
                .send(Frame::VideoFrame {
                    id: emit_id.clone(),
                    from: String::new(),
                    to: emit_to.clone(),
                    seq: n,
                    width: frame.width,
                    height: frame.height,
                    source,
                    data: B64.encode(&frame.jpeg),
                })
                .is_ok()
        };

        let emit = emit;
        let outcome = match source {
            VideoSource::Screen { monitor } => {
                desktop::run_screen_stream(monitor, max_width, quality, fps, stop.clone(), &emit)
            }
            VideoSource::Camera { index } => {
                desktop::run_camera_stream(index, max_width, quality, fps, stop.clone(), |frame| {
                    emit(
                        frame,
                        Geometry {
                            x: 0,
                            y: 0,
                            width: 0,
                            height: 0,
                        },
                    )
                })
            }
        };

        if let Err(e) = outcome {
            let _ = outbound.send(Frame::VideoError {
                id: id.clone(),
                from: String::new(),
                to: requester.clone(),
                message: format!("{e:#}"),
            });
        }
        let _ = outbound.send(Frame::VideoStop {
            id: id.clone(),
            from: String::new(),
            to: requester,
        });
        if let Ok(mut map) = streams.lock() {
            map.remove(&id);
        }
        println!("remote stream {id} ended");
    });
}

fn execute_blocking(op: Operation) -> std::result::Result<Payload, String> {
    match op {
        Operation::Exec { command } => {
            // Blocking shell-out on a dedicated task: the caller already
            // wrapped us in spawn_blocking.
            let out = std::process::Command::new("bash")
                .arg("-lc")
                .arg(command)
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            Ok(Payload::Text {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                code: out.status.code().unwrap_or(128),
            })
        }
        Operation::Upload { path, data } => {
            let bytes = B64.decode(data).map_err(|e| e.to_string())?;
            std::fs::write(&path, bytes).map_err(|e| format!("writing {path}: {e}"))?;
            Ok(Payload::text(format!("Uploaded to {path}\n")))
        }
        Operation::Download { path } => {
            let bytes = std::fs::read(&path).map_err(|e| format!("reading {path}: {e}"))?;
            Ok(Payload::File {
                data: B64.encode(bytes),
            })
        }
        Operation::Screenshot {
            monitor,
            quality,
            max_width,
        } => {
            let (frame, _) = desktop::capture_screen(monitor, max_width, quality)
                .map_err(|e| format!("{e:#}"))?;
            Ok(Payload::Image {
                data: B64.encode(&frame.jpeg),
                width: frame.width,
                height: frame.height,
                format: "jpeg".into(),
            })
        }
        Operation::CameraSnapshot {
            index,
            width,
            quality,
        } => {
            let frame =
                desktop::capture_camera(index, width, quality).map_err(|e| format!("{e:#}"))?;
            Ok(Payload::Image {
                data: B64.encode(&frame.jpeg),
                width: frame.width,
                height: frame.height,
                format: "jpeg".into(),
            })
        }
        Operation::ListScreens => {
            let monitors = desktop::list_monitors().map_err(|e| format!("{e:#}"))?;
            Ok(Payload::Json {
                data: serde_json::to_string(&monitors).map_err(|e| e.to_string())?,
            })
        }
        Operation::ListCameras => {
            let cameras = desktop::list_cameras().map_err(|e| format!("{e:#}"))?;
            Ok(Payload::Json {
                data: serde_json::to_string(&cameras).map_err(|e| e.to_string())?,
            })
        }
        Operation::Ping => Ok(Payload::text(format!(
            "pong {} {} v{}",
            std::env::consts::OS,
            crate::protocol::hostname(),
            env!("CARGO_PKG_VERSION")
        ))),
    }
}

fn start_peer_shell(
    id: String,
    requester: String,
    rows: u16,
    cols: u16,
    term: &str,
    outbound: mpsc::UnboundedSender<Frame>,
    shells: ShellMap,
) -> std::result::Result<(), String> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("opening remote PTY: {e}"))?;
    let mut command = CommandBuilder::new("bash");
    command.arg("-il");
    command.env(
        "TERM",
        if term.is_empty() {
            "xterm-256color"
        } else {
            term
        },
    );
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| format!("starting remote bash: {e}"))?;
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("opening PTY output: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("opening PTY input: {e}"))?;
    let master = pair.master;
    let (control_tx, control_rx) = std_mpsc::channel();
    shells
        .lock()
        .map_err(|_| "shell registry lock poisoned".to_string())?
        .insert(id.clone(), control_tx);
    println!("remote shell {id} opened by '{requester}'");

    let output_tx = outbound.clone();
    let output_id = id.clone();
    let output_to = requester.clone();
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx
                        .send(Frame::ShellOutput {
                            id: output_id.clone(),
                            from: String::new(),
                            to: output_to.clone(),
                            data: B64.encode(&buffer[..n]),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    let shell_id = id.clone();
    thread::spawn(move || {
        let exit_code = loop {
            match control_rx.recv_timeout(Duration::from_millis(40)) {
                Ok(ShellControl::Input(bytes)) => {
                    use std::io::Write as _;
                    if writer.write_all(&bytes).is_err() {
                        let _ = child.kill();
                    }
                }
                Ok(ShellControl::Resize(rows, cols)) => {
                    let _ = master.resize(PtySize {
                        rows: rows.max(1),
                        cols: cols.max(1),
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
                Ok(ShellControl::Close) | Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.exit_code() as i32,
                Ok(None) => {}
                Err(_) => break 1,
            }
        };
        let _ = outbound.send(Frame::ShellExit {
            id: shell_id.clone(),
            from: String::new(),
            to: requester,
            code: exit_code,
        });
        if let Ok(mut registry) = shells.lock() {
            registry.remove(&shell_id);
        }
    });
    Ok(())
}

#[allow(dead_code)]
async fn execute_async(op: Operation) -> std::result::Result<Payload, String> {
    match op {
        Operation::Exec { command } => {
            let out = Command::new("bash")
                .arg("-lc")
                .arg(command)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map_err(|e| e.to_string())?;
            Ok(Payload::Text {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                code: out.status.code().unwrap_or(128),
            })
        }
        other => execute_blocking(other),
    }
}
