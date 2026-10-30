//! Operator-side commands: shell, exec, file transfer, snapshots, live
//! streaming with a browser viewer, and direct input injection.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use tokio::io::BufReader;
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::desktop::{CameraInfo, MonitorInfo};
use crate::httpd::{self, ViewerConfig};
use crate::protocol::{
    read_frame, write_frame, Frame, InputEvent, MouseButton, Operation, Payload, PeerInfo,
    PeerKind, ResponseResult, VideoSource,
};
use crate::relay;

/// A live connection to the relay, acting as an operator.
pub struct OperatorLink {
    pub name: String,
    tx: mpsc::UnboundedSender<Frame>,
    reader: BufReader<OwnedReadHalf>,
}

impl OperatorLink {
    pub async fn connect(server: &str, password: Option<String>) -> Result<Self> {
        let stream = TcpStream::connect(server)
            .await
            .with_context(|| format!("connecting to {server}"))?;
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let name = format!("operator-{}", Uuid::new_v4());
        write_frame(
            &mut write,
            &Frame::Hello {
                name: name.clone(),
                password,
                kind: PeerKind::Operator,
                info: None,
            },
        )
        .await?;
        match read_frame(&mut reader).await? {
            Frame::Welcome => {}
            Frame::Error { message } => bail!("server rejected connection: {message}"),
            _ => bail!("unexpected server response"),
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if write_frame(&mut write, &frame).await.is_err() {
                    break;
                }
            }
        });
        Ok(Self { name, tx, reader })
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<Frame> {
        self.tx.clone()
    }

    pub fn send(&self, frame: Frame) {
        let _ = self.tx.send(frame);
    }

    pub async fn recv(&mut self) -> Result<Frame> {
        read_frame(&mut self.reader).await
    }

    /// Send a request and wait for the matching response, ignoring unrelated
    /// frames (a live stream can be interleaved with request traffic).
    pub async fn request(&mut self, target: &str, op: Operation) -> Result<Payload> {
        let id = Uuid::new_v4().to_string();
        self.send(Frame::Request {
            id: id.clone(),
            from: self.name.clone(),
            target: target.to_owned(),
            op,
        });
        loop {
            match self.recv().await? {
                Frame::Response {
                    id: response_id,
                    result,
                    ..
                } if response_id == id => return result.into_result().map_err(|e| anyhow!(e)),
                Frame::VideoError { message, .. } => bail!("{message}"),
                Frame::Error { message } => bail!("server error: {message}"),
                _ => continue,
            }
        }
    }

    pub async fn list_peers(&mut self) -> Result<Vec<PeerInfo>> {
        let id = Uuid::new_v4().to_string();
        self.send(Frame::ListPeers {
            id: id.clone(),
            from: self.name.clone(),
        });
        loop {
            match self.recv().await? {
                Frame::PeerList {
                    id: response_id,
                    peers,
                    ..
                } if response_id == id => return Ok(peers),
                Frame::Error { message } => bail!("server error: {message}"),
                _ => continue,
            }
        }
    }
}

// ------------------------------------------------------------------- peers ---

pub async fn run_peers(server: &str, password: Option<String>) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let peers = link.list_peers().await?;
    print!("{}", relay::describe_peers(&peers));
    Ok(())
}

// -------------------------------------------------------------- one-shots ---

pub async fn run_exec(
    server: &str,
    password: Option<String>,
    target: &str,
    command: String,
) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    match link.request(target, Operation::Exec { command }).await? {
        Payload::Text {
            stdout,
            stderr,
            code,
        } => {
            print!("{stdout}");
            eprint!("{stderr}");
            if code != 0 {
                bail!("remote bash exited with status {code}");
            }
            Ok(())
        }
        _ => bail!("unexpected response from peer"),
    }
}

pub async fn run_upload(
    server: &str,
    password: Option<String>,
    target: &str,
    local: PathBuf,
    remote: PathBuf,
) -> Result<()> {
    let bytes = tokio::fs::read(&local)
        .await
        .with_context(|| format!("reading {}", local.display()))?;
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link
        .request(
            target,
            Operation::Upload {
                path: remote.to_string_lossy().into_owned(),
                data: B64.encode(bytes),
            },
        )
        .await?;
    expect_text(payload)
}

pub async fn run_download(
    server: &str,
    password: Option<String>,
    target: &str,
    remote: PathBuf,
    local: PathBuf,
) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link
        .request(
            target,
            Operation::Download {
                path: remote.to_string_lossy().into_owned(),
            },
        )
        .await?;
    match payload {
        Payload::File { data } => {
            let bytes = B64.decode(data).context("invalid file payload")?;
            tokio::fs::write(&local, bytes)
                .await
                .with_context(|| format!("writing {}", local.display()))?;
            println!("Downloaded to {}", local.display());
            Ok(())
        }
        _ => bail!("unexpected response from peer"),
    }
}

fn expect_text(payload: Payload) -> Result<()> {
    match payload {
        Payload::Text {
            stdout,
            stderr,
            code,
        } => {
            print!("{stdout}");
            eprint!("{stderr}");
            if code != 0 {
                bail!("remote operation exited with status {code}");
            }
            Ok(())
        }
        other => bail!("unexpected response from peer: {other:?}"),
    }
}

pub async fn run_screenshot(
    server: &str,
    password: Option<String>,
    target: &str,
    monitor: u32,
    quality: u8,
    max_width: u32,
    output: &Path,
) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link
        .request(
            target,
            Operation::Screenshot {
                monitor,
                quality,
                max_width,
            },
        )
        .await?;
    save_image(payload, output)
}

pub async fn run_camera_snapshot(
    server: &str,
    password: Option<String>,
    target: &str,
    index: u32,
    width: u32,
    quality: u8,
    output: &Path,
) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link
        .request(
            target,
            Operation::CameraSnapshot {
                index,
                width,
                quality,
            },
        )
        .await?;
    save_image(payload, output)
}

fn save_image(payload: Payload, output: &Path) -> Result<()> {
    match payload {
        Payload::Image {
            data,
            width,
            height,
            format,
        } => {
            let bytes = B64.decode(data).context("invalid image payload")?;
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).ok();
                }
            }
            std::fs::write(output, &bytes)
                .with_context(|| format!("writing {}", output.display()))?;
            println!(
                "Saved {}x{} {} ({} bytes) to {}",
                width,
                height,
                format,
                bytes.len(),
                output.display()
            );
            Ok(())
        }
        other => bail!("unexpected response from peer: {other:?}"),
    }
}

pub async fn run_screens(server: &str, password: Option<String>, target: &str) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link.request(target, Operation::ListScreens).await?;
    match payload {
        Payload::Json { data } => {
            let monitors: Vec<MonitorInfo> = serde_json::from_str(&data)?;
            if monitors.is_empty() {
                println!("no monitors reported");
                return Ok(());
            }
            println!("{:<4} {:<24} {:<12} size", "idx", "name", "position");
            for m in monitors {
                println!(
                    "{:<4} {:<24} {:<12} {}x{} @{:.2}x{}",
                    m.index,
                    m.name,
                    format!("{}+{}", m.x, m.y),
                    m.width,
                    m.height,
                    m.scale,
                    if m.primary { " primary" } else { "" }
                );
            }
            Ok(())
        }
        other => bail!("unexpected response from peer: {other:?}"),
    }
}

pub async fn run_cameras(server: &str, password: Option<String>, target: &str) -> Result<()> {
    let mut link = OperatorLink::connect(server, password).await?;
    let payload = link.request(target, Operation::ListCameras).await?;
    match payload {
        Payload::Json { data } => {
            let cameras: Vec<CameraInfo> = serde_json::from_str(&data)?;
            if cameras.is_empty() {
                println!("no webcams reported");
                return Ok(());
            }
            for c in cameras {
                println!("{}: {}", c.index, c.name);
            }
            Ok(())
        }
        other => bail!("unexpected response from peer: {other:?}"),
    }
}

// ---------------------------------------------------------------- streaming ---

pub struct StreamOptions {
    pub server: String,
    pub password: Option<String>,
    pub target: String,
    pub source: VideoSource,
    pub fps: u8,
    pub quality: u8,
    pub max_width: u32,
    pub bind: Option<String>,
    pub record: Option<PathBuf>,
    pub duration: Option<u64>,
    pub allow_input: bool,
}

pub async fn run_stream(options: StreamOptions) -> Result<()> {
    let StreamOptions {
        server,
        password,
        target,
        source,
        fps,
        quality,
        max_width,
        bind,
        record,
        duration,
        allow_input,
    } = options;

    let link = OperatorLink::connect(&server, password).await?;
    let stream_id = Uuid::new_v4().to_string();
    let (frames_tx, _) = broadcast::channel::<Arc<Vec<u8>>>(32);
    let latest: Arc<Mutex<Option<Arc<Vec<u8>>>>> = Arc::new(Mutex::new(None));
    let input_events = Arc::new(AtomicU64::new(0));

    if let Some(bind) = bind.clone() {
        let cfg = ViewerConfig {
            bind: bind.clone(),
            title: format!("{target} — {}", describe_source(source)),
            target: target.clone(),
            stream_id: stream_id.clone(),
            frames: frames_tx.clone(),
            latest: latest.clone(),
            outbound: link.sender(),
            allow_input,
            input_events: input_events.clone(),
        };
        tokio::spawn(async move {
            if let Err(e) = httpd::serve(cfg).await {
                eprintln!("viewer stopped: {e:#}");
            }
        });
        let shown = if bind.starts_with("0.0.0.0") || bind.contains("127.0.0.1") {
            bind.replace("0.0.0.0", "127.0.0.1")
        } else {
            bind.clone()
        };
        println!("Live viewer: http://{shown}/  (Ctrl-C to stop)");
        if allow_input && !bind.starts_with("127.0.0.1") && !bind.starts_with("localhost") {
            eprintln!(
                "WARNING: the viewer is reachable beyond loopback; anyone who can open that URL can drive this machine."
            );
        }
    }

    if let Some(dir) = &record {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let mut link = link;
    link.send(Frame::VideoStart {
        id: stream_id.clone(),
        from: link.name.clone(),
        target: target.clone(),
        source,
        max_width,
        quality,
        fps,
    });

    let deadline = duration.map(|secs| tokio::time::Instant::now() + Duration::from_secs(secs));
    let mut seq_count = 0u64;
    let mut bytes_total = 0u64;
    let mut stopped = false;
    let record_dir = record.clone();

    loop {
        tokio::select! {
            frame = link.recv() => {
                match frame? {
                    Frame::VideoFrame { id, data, width, height, seq, .. } if id == stream_id => {
                        let jpeg = B64.decode(data).context("invalid frame payload")?;
                        bytes_total += jpeg.len() as u64;
                        seq_count += 1;
                        if let Some(dir) = &record_dir {
                            let path = dir.join(format!("frame_{seq:06}.jpg"));
                            let _ = std::fs::write(path, &jpeg);
                        }
                        if record.is_none() {
                            // Progress line even without a viewer attached.
                            if seq_count.is_multiple_of(30) {
                                eprintln!(
                                    "frame {seq_count} ({width}x{height}, {:.0} KiB/frame avg)",
                                    bytes_total as f64 / seq_count as f64 / 1024.0
                                );
                            }
                        }
                        let jpeg = Arc::new(jpeg);
                        *latest.lock().unwrap() = Some(jpeg.clone());
                        let _ = frames_tx.send(jpeg);
                    }
                    Frame::VideoError { id, message, .. } if id == stream_id => {
                        bail!("{message}");
                    }
                    Frame::VideoStop { id, .. } if id == stream_id => {
                        stopped = true;
                        break;
                    }
                    Frame::Error { message } => eprintln!("server: {message}"),
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\nstopping stream");
                break;
            }
            _ = wait_for_deadline(deadline) => {
                println!("duration reached ({seq_count} frames)");
                break;
            }
        }
    }

    if !stopped {
        link.send(Frame::VideoStop {
            id: stream_id.clone(),
            from: link.name.clone(),
            to: target.clone(),
        });
        // Give the writer task a moment to flush before we drop the socket.
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    let interactive = input_events.load(Ordering::Relaxed);
    println!(
        "stream closed: {seq_count} frames, {:.1} KiB total{}",
        bytes_total as f64 / 1024.0,
        if interactive > 0 {
            format!(", {interactive} input event(s) injected")
        } else {
            String::new()
        }
    );
    Ok(())
}

async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn describe_source(source: VideoSource) -> String {
    match source {
        VideoSource::Screen { monitor } => format!("screen {monitor}"),
        VideoSource::Camera { index } => format!("webcam {index}"),
    }
}

// ------------------------------------------------------------------- input ---

pub async fn run_input(
    server: &str,
    password: Option<String>,
    target: &str,
    events: Vec<InputEvent>,
) -> Result<()> {
    if events.is_empty() {
        bail!("no input events given (see --help)");
    }
    let mut link = OperatorLink::connect(server, password).await?;
    let id = Uuid::new_v4().to_string();
    link.send(Frame::Control {
        id: id.clone(),
        from: link.name.clone(),
        target: target.to_owned(),
        stream: None,
        events,
    });
    loop {
        match link.recv().await? {
            Frame::Response {
                id: response_id,
                result,
                ..
            } if response_id == id => {
                return match result.into_result() {
                    Ok(Payload::Text { stdout, .. }) => {
                        println!("{}", stdout.trim());
                        Ok(())
                    }
                    Ok(_) => Ok(()),
                    Err(e) => Err(anyhow!(e)),
                };
            }
            Frame::Error { message } => bail!("server error: {message}"),
            _ => continue,
        }
    }
}

/// Build a single click/scroll/key event from CLI flags, used by `reaven input`.
#[allow(clippy::too_many_arguments)]
pub fn build_input_events(
    point: Option<(f32, f32)>,
    click: Option<MouseButton>,
    double: bool,
    scroll: Option<i32>,
    scroll_x: Option<i32>,
    key: Option<String>,
    text: Option<String>,
    combo: Option<String>,
) -> Vec<InputEvent> {
    let mut events = Vec::new();
    let (x, y) = point.unwrap_or((0.5, 0.5));
    if let Some(button) = click {
        events.push(if double {
            InputEvent::DoubleClick { button, x, y }
        } else {
            InputEvent::Click { button, x, y }
        });
    }
    if scroll.is_some() || scroll_x.is_some() {
        events.push(InputEvent::Scroll {
            x,
            y,
            dx: scroll_x.unwrap_or(0),
            dy: scroll.unwrap_or(0),
        });
    }
    if let Some(key) = key {
        for (pressed, on) in [(true, true), (false, false)] {
            let _ = on;
            events.push(InputEvent::Key {
                key: key.clone(),
                pressed,
            });
        }
    }
    if let Some(text) = text {
        events.push(InputEvent::Text { text });
    }
    if let Some(combo) = combo {
        events.push(InputEvent::Combo {
            keys: combo
                .split('+')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        });
    }
    events
}

// ------------------------------------------------------------------- shell ---

enum LocalInput {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Close,
}

struct RawModeGuard;
impl RawModeGuard {
    fn enable() -> Result<Self> {
        terminal::enable_raw_mode().context("enabling terminal raw mode")?;
        Ok(Self)
    }
}
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

pub async fn run_cl(server: &str, password: Option<String>, target: &str) -> Result<()> {
    let (cols, rows) = terminal::size().context("reading local terminal size")?;
    let _raw_mode = RawModeGuard::enable()?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    let stream = TcpStream::connect(server)
        .await
        .with_context(|| format!("connecting to {server}"))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let peer_name = format!("operator-shell-{}", Uuid::new_v4());
    write_frame(
        &mut write,
        &Frame::Hello {
            name: peer_name,
            password,
            kind: PeerKind::Operator,
            info: None,
        },
    )
    .await?;
    match read_frame(&mut reader).await? {
        Frame::Welcome => {}
        Frame::Error { message } => bail!("server rejected connection: {message}"),
        _ => bail!("unexpected server response"),
    }
    let id = Uuid::new_v4().to_string();
    write_frame(
        &mut write,
        &Frame::ShellStart {
            id: id.clone(),
            from: String::new(),
            target: target.to_string(),
            rows,
            cols,
            term,
        },
    )
    .await?;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Frame>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if write_frame(&mut write, &frame).await.is_err() {
                break;
            }
        }
    });
    let running = Arc::new(AtomicBool::new(true));
    let input_running = running.clone();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<LocalInput>();
    let input_thread = thread::spawn(move || {
        while input_running.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if is_detach_key(key) {
                            let _ = input_tx.send(LocalInput::Close);
                            break;
                        }
                        if let Some(bytes) = encode_key(key) {
                            if input_tx.send(LocalInput::Bytes(bytes)).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(Event::Resize(cols, rows)) => {
                        if input_tx.send(LocalInput::Resize(rows, cols)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Paste(text)) => {
                        if input_tx.send(LocalInput::Bytes(text.into_bytes())).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });

    let result: Result<()> = async {
        loop {
            tokio::select! {
                frame = read_frame(&mut reader) => {
                    match frame? {
                        Frame::ShellOutput { id: frame_id, data, .. } if frame_id == id => {
                            let bytes = B64.decode(data).context("invalid shell output frame")?;
                            let mut stdout = std::io::stdout().lock();
                            stdout.write_all(&bytes)?;
                            stdout.flush()?;
                        }
                        Frame::ShellExit { id: frame_id, code, .. } if frame_id == id => {
                            let mut stdout = std::io::stdout().lock();
                            write!(stdout, "\r\n[remote shell exited: {code}]\r\n")?;
                            stdout.flush()?;
                            break Ok(());
                        }
                        Frame::Response { id: frame_id, result: ResponseResult::Err(message), .. } if frame_id == id => {
                            bail!("could not open remote shell: {message}")
                        }
                        Frame::Error { message } => bail!("server error: {message}"),
                        _ => {}
                    }
                }
                event = input_rx.recv() => {
                    match event {
                        Some(LocalInput::Bytes(bytes)) => {
                            let _ = out_tx.send(Frame::ShellInput {
                                id: id.clone(),
                                from: String::new(),
                                to: target.to_string(),
                                data: B64.encode(bytes),
                            });
                        }
                        Some(LocalInput::Resize(rows, cols)) => {
                            let _ = out_tx.send(Frame::ShellResize {
                                id: id.clone(),
                                from: String::new(),
                                to: target.to_string(),
                                rows,
                                cols,
                            });
                        }
                        Some(LocalInput::Close) => {
                            let _ = out_tx.send(Frame::ShellClose {
                                id: id.clone(),
                                from: String::new(),
                                to: target.to_string(),
                            });
                            break Ok(());
                        }
                        None => bail!("terminal input ended"),
                    }
                }
            }
        }
    }
    .await;
    running.store(false, Ordering::Relaxed);
    let _ = out_tx.send(Frame::ShellClose {
        id,
        from: String::new(),
        to: target.to_string(),
    });
    let _ = input_thread.join();
    writer.abort();
    result
}

fn is_detach_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(']')) && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut bytes = match key.code {
        KeyCode::Char(ch) if control => {
            let ch = ch.to_ascii_lowercase();
            let value = match ch {
                '@' | ' ' => 0,
                'a'..='z' => ch as u8 - b'a' + 1,
                '[' => 27,
                '\\' => 28,
                ']' => 29,
                '^' => 30,
                '_' => 31,
                _ => return None,
            };
            vec![value]
        }
        KeyCode::Char(ch) => ch.to_string().into_bytes(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(n @ 5..=12) => format!("\x1b[{}~", 15 + (n - 5) * 3).into_bytes(),
        _ => return None,
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}
