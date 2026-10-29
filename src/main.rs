use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self as std_io, Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc, Arc, Mutex as StdMutex,
    },
    thread,
    time::Duration,
};
use tokio::{
    fs,
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{mpsc, Mutex},
    time::timeout,
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "reaven",
    version,
    about = "Server-relayed remote shell and file transfer"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the relay server. Keep this port private or protect it with a firewall/VPN.
    Server {
        #[arg(long, default_value = "0.0.0.0:7331")]
        listen: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Register this computer as an online peer (run in the background for availability).
    Connect {
        #[arg(long)]
        server: String,
        #[arg(long)]
        name: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Open a full interactive remote shell (PTY; colors, line editing, Ctrl-C, job control).
    Cl {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Run a bash command on an online peer.
    Exec {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long)]
        command: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Copy a local file to a peer.
    Upload {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        local: PathBuf,
        remote: PathBuf,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Copy a file from a peer to this computer.
    Download {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        remote: PathBuf,
        local: PathBuf,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Add an idempotent client start line to ~/.bashrc.
    InstallAutostart {
        #[arg(long)]
        server: String,
        #[arg(long)]
        name: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame {
    Hello {
        name: String,
        password: Option<String>,
    },
    Welcome,
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
        result: std::result::Result<Payload, String>,
    },
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
    Error {
        message: String,
    },
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Operation {
    Exec { command: String },
    Upload { path: String, data: String },
    Download { path: String },
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Payload {
    Text {
        stdout: String,
        stderr: String,
        code: i32,
    },
    File {
        data: String,
    },
}

type PeerMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Frame>>>>;
type ShellMap = Arc<StdMutex<HashMap<String, std_mpsc::Sender<ShellControl>>>>;

enum ShellControl {
    Input(Vec<u8>),
    Resize(u16, u16),
    Close,
}
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Server { listen, password } => run_server(&listen, password).await,
        Commands::Connect {
            server,
            name,
            password,
        } => run_peer(&server, &name, password).await,
        Commands::Cl {
            server,
            target,
            password,
        } => run_cl(&server, password, &target).await,
        Commands::Exec {
            server,
            target,
            command,
            password,
        } => match send_request(&server, password, &target, Operation::Exec { command }).await? {
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
        },
        Commands::Upload {
            server,
            target,
            local,
            remote,
            password,
        } => {
            let bytes = fs::read(&local)
                .await
                .with_context(|| format!("reading {}", local.display()))?;
            let op = Operation::Upload {
                path: remote.to_string_lossy().into_owned(),
                data: B64.encode(bytes),
            };
            expect_text(send_request(&server, password, &target, op).await?)
        }
        Commands::Download {
            server,
            target,
            remote,
            local,
            password,
        } => {
            match send_request(
                &server,
                password,
                &target,
                Operation::Download {
                    path: remote.to_string_lossy().into_owned(),
                },
            )
            .await?
            {
                Payload::File { data } => {
                    let bytes = B64.decode(data).context("invalid file payload")?;
                    fs::write(&local, bytes)
                        .await
                        .with_context(|| format!("writing {}", local.display()))?;
                    println!("Downloaded to {}", local.display());
                    Ok(())
                }
                _ => bail!("unexpected response from peer"),
            }
        }
        Commands::InstallAutostart {
            server,
            name,
            password,
        } => install_autostart(&server, &name, password.as_deref()),
    }
}

fn expect_text(p: Payload) -> Result<()> {
    match p {
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
        _ => bail!("unexpected response from peer"),
    }
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> Result<()> {
    let mut line = serde_json::to_vec(frame)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}
async fn read_frame<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Frame> {
    let mut line = String::new();
    let n = r.read_line(&mut line).await?;
    if n == 0 {
        bail!("connection closed");
    }
    Ok(serde_json::from_str(&line).context("invalid protocol frame")?)
}

async fn run_server(addr: &str, password: Option<String>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    println!(
        "Reaven relay listening on {addr}{}",
        if password.is_some() {
            " (password required)"
        } else {
            " (NO PASSWORD)"
        }
    );
    let peers: PeerMap = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let (stream, remote) = listener.accept().await?;
        let map = peers.clone();
        let pass = password.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_connection(stream, map, pass).await {
                eprintln!("{remote}: {e:#}");
            }
        });
    }
}
async fn serve_connection(
    stream: TcpStream,
    peers: PeerMap,
    password: Option<String>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let name = match timeout(Duration::from_secs(10), read_frame(&mut reader))
        .await
        .context("waiting for hello")??
    {
        Frame::Hello {
            name,
            password: supplied,
        } => {
            if password != supplied {
                write_frame(
                    &mut write,
                    &Frame::Error {
                        message: "authentication failed".into(),
                    },
                )
                .await?;
                bail!("authentication failed");
            }
            name
        }
        _ => bail!("expected hello"),
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    {
        let mut map = peers.lock().await;
        if let Some(old) = map.insert(name.clone(), tx.clone()) {
            let _ = old.send(Frame::Error {
                message: "another connection used this peer name".into(),
            });
        }
    }
    write_frame(&mut write, &Frame::Welcome).await?;
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_frame(&mut write, &frame).await.is_err() {
                break;
            }
        }
    });
    let loop_result: Result<()> = async {
        loop {
            match read_frame(&mut reader).await? {
                Frame::Request { id, target, op, .. } => {
                    let dest = peers.lock().await.get(&target).cloned();
                    if let Some(dest) = dest {
                        let _ = dest.send(Frame::Request {
                            id,
                            from: name.clone(),
                            target,
                            op,
                        });
                    } else {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            result: Err("target is offline".into()),
                        });
                    }
                }
                Frame::Response { id, to, result, .. } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::Response {
                            id,
                            from: name.clone(),
                            to,
                            result,
                        });
                    }
                }
                Frame::ShellStart {
                    id,
                    target,
                    rows,
                    cols,
                    term,
                    ..
                } => {
                    if let Some(dest) = peers.lock().await.get(&target).cloned() {
                        let _ = dest.send(Frame::ShellStart {
                            id,
                            from: name.clone(),
                            target,
                            rows,
                            cols,
                            term,
                        });
                    } else {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            result: Err("target is offline".into()),
                        });
                    }
                }
                Frame::ShellInput { id, to, data, .. } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::ShellInput {
                            id,
                            from: name.clone(),
                            to,
                            data,
                        });
                    }
                }
                Frame::ShellResize {
                    id, to, rows, cols, ..
                } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::ShellResize {
                            id,
                            from: name.clone(),
                            to,
                            rows,
                            cols,
                        });
                    }
                }
                Frame::ShellClose { id, to, .. } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::ShellClose {
                            id,
                            from: name.clone(),
                            to,
                        });
                    }
                }
                Frame::ShellOutput { id, to, data, .. } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::ShellOutput {
                            id,
                            from: name.clone(),
                            to,
                            data,
                        });
                    }
                }
                Frame::ShellExit { id, to, code, .. } => {
                    if let Some(dest) = peers.lock().await.get(&to).cloned() {
                        let _ = dest.send(Frame::ShellExit {
                            id,
                            from: name.clone(),
                            to,
                            code,
                        });
                    }
                }
                Frame::Error { message } => eprintln!("peer {name}: {message}"),
                _ => bail!("unexpected client frame"),
            }
        }
    }
    .await;
    {
        let mut map = peers.lock().await;
        if map
            .get(&name)
            .is_some_and(|current| current.same_channel(&tx))
        {
            map.remove(&name);
        }
    }
    writer.abort();
    loop_result
}

async fn run_peer(server: &str, name: &str, password: Option<String>) -> Result<()> {
    let stream = TcpStream::connect(server)
        .await
        .with_context(|| format!("connecting to {server}"))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write_frame(
        &mut write,
        &Frame::Hello {
            name: name.to_owned(),
            password,
        },
    )
    .await?;
    match read_frame(&mut reader).await? {
        Frame::Welcome => println!("Reaven peer '{}' online at {}", name, server),
        Frame::Error { message } => bail!("server rejected connection: {message}"),
        _ => bail!("unexpected server response"),
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    let shells: ShellMap = Arc::new(StdMutex::new(HashMap::new()));
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_frame(&mut write, &frame).await.is_err() {
                break;
            }
        }
    });
    let result: Result<()> = async {
        loop {
            match read_frame(&mut reader).await? {
                Frame::Request { id, from, op, .. } => {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = execute(op).await;
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
                            result: Err(message),
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
                Frame::Error { message } => eprintln!("Reaven: {message}"),
                _ => bail!("unexpected server frame"),
            }
        }
    }
    .await;
    writer.abort();
    result
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

async fn run_cl(server: &str, password: Option<String>, target: &str) -> Result<()> {
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
                            let mut stdout = std_io::stdout().lock();
                            stdout.write_all(&bytes)?;
                            stdout.flush()?;
                        }
                        Frame::ShellExit { id: frame_id, code, .. } if frame_id == id => {
                            let mut stdout = std_io::stdout().lock();
                            write!(stdout, "\r\n[remote shell exited: {code}]\r\n")?;
                            stdout.flush()?;
                            break Ok(());
                        }
                        Frame::Response { id: frame_id, result: Err(message), .. } if frame_id == id => bail!("could not open remote shell: {message}"),
                        Frame::Error { message } => bail!("server error: {message}"),
                        _ => {}
                    }
                }
                event = input_rx.recv() => {
                    match event {
                        Some(LocalInput::Bytes(bytes)) => {
                            let _ = out_tx.send(Frame::ShellInput { id: id.clone(), from: String::new(), to: target.to_string(), data: B64.encode(bytes) });
                        }
                        Some(LocalInput::Resize(rows, cols)) => {
                            let _ = out_tx.send(Frame::ShellResize { id: id.clone(), from: String::new(), to: target.to_string(), rows, cols });
                        }
                        Some(LocalInput::Close) => {
                            let _ = out_tx.send(Frame::ShellClose { id: id.clone(), from: String::new(), to: target.to_string() });
                            break Ok(());
                        }
                        None => bail!("terminal input ended"),
                    }
                }
            }
        }
    }.await;
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

async fn execute(op: Operation) -> std::result::Result<Payload, String> {
    match op {
        Operation::Exec { command } => {
            let out = Command::new("bash")
                .arg("-lc")
                .arg(command)
                .stdin(Stdio::null())
                .output()
                .await
                .map_err(|e| e.to_string())?;
            Ok(Payload::Text {
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                code: out.status.code().unwrap_or(128),
            })
        }
        Operation::Upload { path, data } => {
            let bytes = B64.decode(data).map_err(|e| e.to_string())?;
            fs::write(&path, bytes)
                .await
                .map_err(|e| format!("writing {path}: {e}"))?;
            Ok(Payload::Text {
                stdout: format!("Uploaded to {path}\n"),
                stderr: String::new(),
                code: 0,
            })
        }
        Operation::Download { path } => {
            let bytes = fs::read(&path)
                .await
                .map_err(|e| format!("reading {path}: {e}"))?;
            Ok(Payload::File {
                data: B64.encode(bytes),
            })
        }
    }
}

async fn send_request(
    server: &str,
    password: Option<String>,
    target: &str,
    op: Operation,
) -> Result<Payload> {
    let stream = TcpStream::connect(server)
        .await
        .with_context(|| format!("connecting to {server}"))?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write_frame(
        &mut write,
        &Frame::Hello {
            name: format!("operator-{}", Uuid::new_v4()),
            password,
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
        &Frame::Request {
            id: id.clone(),
            from: "operator".into(),
            target: target.into(),
            op,
        },
    )
    .await?;
    loop {
        match timeout(Duration::from_secs(300), read_frame(&mut reader))
            .await
            .context("timed out waiting for peer")??
        {
            Frame::Response {
                id: response_id,
                result,
                ..
            } if response_id == id => return result.map_err(|e| anyhow!(e)),
            Frame::Error { message } => bail!("server error: {message}"),
            _ => continue,
        }
    }
}

fn install_autostart(server: &str, name: &str, password: Option<&str>) -> Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let path = PathBuf::from(home).join(".bashrc");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();
    const BEGIN: &str = "# >>> reaven autostart >>>";
    const END: &str = "# <<< reaven autostart <<<";
    if content.contains(BEGIN) {
        println!(
            "Reaven autostart is already present in {} (unchanged)",
            path.display()
        );
        return Ok(());
    }
    let exe = std::env::current_exe().context("locating reaven executable")?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let args = format!(
        "{} connect --server {} --name {}",
        quote(&exe.to_string_lossy()),
        quote(server),
        quote(name)
    );
    let env_prefix = password
        .map(|p| format!("REAVEN_PASSWORD={} ", quote(p)))
        .unwrap_or_default();
    let block = format!("\n{BEGIN}\nif [[ $- == *i* ]] && command -v flock >/dev/null 2>&1; then\n  (flock -n \"$HOME/.reaven.lock\" env {env_prefix}{args} >> \"$HOME/.reaven.log\" 2>&1 &)\nfi\n{END}\n");
    content.push_str(&block);
    std::fs::write(&path, content).with_context(|| format!("updating {}", path.display()))?;
    println!(
        "Installed idempotent Reaven autostart in {}",
        path.display()
    );
    println!("Note: the password, if supplied, is stored in plaintext in .bashrc.");
    Ok(())
}
