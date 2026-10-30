//! The relay: accepts authenticated peers and routes frames between them by name.
//!
//! The relay never interprets pixel data or input events; it only rewrites the
//! `from` field (so a peer cannot impersonate another) and forwards. That keeps
//! the hot streaming path cheap and means the relay holds no session state that
//! could leak between operators.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

use crate::protocol::{
    read_frame, write_frame, Frame, PeerInfo, PeerKind, ResponseResult, PROTOCOL_VERSION,
};

struct PeerEntry {
    tx: mpsc::UnboundedSender<Frame>,
    info: PeerInfo,
    kind: PeerKind,
}

type PeerMap = Arc<Mutex<HashMap<String, PeerEntry>>>;

pub async fn run_server(addr: &str, password: Option<String>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    println!(
        "Reaven relay v{} (protocol v{}) listening on {addr}{}",
        env!("CARGO_PKG_VERSION"),
        PROTOCOL_VERSION,
        if password.is_some() {
            " (password required)"
        } else {
            " (NO PASSWORD -- do not expose this to an untrusted network)"
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

async fn send_to(peers: &PeerMap, name: &str) -> Option<mpsc::UnboundedSender<Frame>> {
    peers.lock().await.get(name).map(|p| p.tx.clone())
}

async fn serve_connection(
    stream: TcpStream,
    peers: PeerMap,
    password: Option<String>,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let (name, kind, mut info) = match timeout(Duration::from_secs(10), read_frame(&mut reader))
        .await
        .context("waiting for hello")??
    {
        Frame::Hello {
            name,
            password: supplied,
            kind,
            info,
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
            if name.trim().is_empty() {
                bail!("empty peer name");
            }
            let info = info.unwrap_or_else(|| PeerInfo::local(&name, kind));
            (name, kind, info)
        }
        _ => bail!("expected hello"),
    };
    info.name = name.clone();
    info.kind = kind;

    let (tx, mut rx) = mpsc::unbounded_channel::<Frame>();
    {
        let mut map = peers.lock().await;
        if let Some(old) = map.insert(
            name.clone(),
            PeerEntry {
                tx: tx.clone(),
                info: info.clone(),
                kind,
            },
        ) {
            let _ = old.tx.send(Frame::Error {
                message: "another connection used this peer name".into(),
            });
        }
    }
    if kind == PeerKind::Agent {
        println!(
            "peer '{name}' online ({}, {})",
            info.hostname,
            if info.os.is_empty() {
                "unknown os"
            } else {
                &info.os
            }
        );
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
            let frame = read_frame(&mut reader).await?;
            match frame {
                Frame::Request { id, target, op, .. } => match send_to(&peers, &target).await {
                    Some(dest) => {
                        let _ = dest.send(Frame::Request {
                            id,
                            from: name.clone(),
                            target,
                            op,
                        });
                    }
                    None => {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            result: ResponseResult::Err(format!("'{target}' is offline")),
                        });
                    }
                },
                Frame::Response { id, to, result, .. } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::Response {
                            id,
                            from: name.clone(),
                            to,
                            result,
                        });
                    }
                }

                // ---- discovery ----
                Frame::ListPeers { id, .. } => {
                    let agents: Vec<PeerInfo> = peers
                        .lock()
                        .await
                        .values()
                        .filter(|p| p.kind == PeerKind::Agent)
                        .map(|p| p.info.clone())
                        .collect();
                    let _ = tx.send(Frame::PeerList {
                        id,
                        to: name.clone(),
                        peers: agents,
                    });
                }

                // ---- shell ----
                Frame::ShellStart {
                    id,
                    target,
                    rows,
                    cols,
                    term,
                    ..
                } => match send_to(&peers, &target).await {
                    Some(dest) => {
                        let _ = dest.send(Frame::ShellStart {
                            id,
                            from: name.clone(),
                            target,
                            rows,
                            cols,
                            term,
                        });
                    }
                    None => {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            result: ResponseResult::Err(format!("'{target}' is offline")),
                        });
                    }
                },
                Frame::ShellInput { id, to, data, .. } => {
                    if let Some(dest) = send_to(&peers, &to).await {
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
                    if let Some(dest) = send_to(&peers, &to).await {
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
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::ShellClose {
                            id,
                            from: name.clone(),
                            to,
                        });
                    }
                }
                Frame::ShellOutput { id, to, data, .. } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::ShellOutput {
                            id,
                            from: name.clone(),
                            to,
                            data,
                        });
                    }
                }
                Frame::ShellExit { id, to, code, .. } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::ShellExit {
                            id,
                            from: name.clone(),
                            to,
                            code,
                        });
                    }
                }

                // ---- video streaming ----
                Frame::VideoStart {
                    id,
                    target,
                    source,
                    max_width,
                    quality,
                    fps,
                    ..
                } => match send_to(&peers, &target).await {
                    Some(dest) => {
                        let _ = dest.send(Frame::VideoStart {
                            id,
                            from: name.clone(),
                            target,
                            source,
                            max_width,
                            quality,
                            fps,
                        });
                    }
                    None => {
                        let _ = tx.send(Frame::VideoError {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            message: format!("'{target}' is offline"),
                        });
                    }
                },
                Frame::VideoStop { id, to, .. } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::VideoStop {
                            id,
                            from: name.clone(),
                            to,
                        });
                    }
                }
                Frame::VideoFrame {
                    id,
                    to,
                    seq,
                    width,
                    height,
                    source,
                    data,
                    ..
                } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::VideoFrame {
                            id,
                            from: name.clone(),
                            to,
                            seq,
                            width,
                            height,
                            source,
                            data,
                        });
                    }
                }
                Frame::VideoError {
                    id, to, message, ..
                } => {
                    if let Some(dest) = send_to(&peers, &to).await {
                        let _ = dest.send(Frame::VideoError {
                            id,
                            from: name.clone(),
                            to,
                            message,
                        });
                    }
                }

                // ---- input injection ----
                Frame::Control {
                    id,
                    target,
                    stream,
                    events,
                    ..
                } => match send_to(&peers, &target).await {
                    Some(dest) => {
                        let _ = dest.send(Frame::Control {
                            id,
                            from: name.clone(),
                            target,
                            stream,
                            events,
                        });
                    }
                    None => {
                        let _ = tx.send(Frame::Response {
                            id,
                            from: "reaven-server".into(),
                            to: name.clone(),
                            result: ResponseResult::Err(format!("'{target}' is offline")),
                        });
                    }
                },

                Frame::Error { message } => eprintln!("peer {name}: {message}"),
                other => bail!("unexpected client frame: {other:?}"),
            }
        }
    }
    .await;

    {
        let mut map = peers.lock().await;
        let still_me = map
            .get(&name)
            .map(|entry| entry.tx.same_channel(&tx))
            .unwrap_or(false);
        if still_me {
            map.remove(&name);
            if kind == PeerKind::Agent {
                println!("peer '{name}' offline");
            }
        }
    }
    writer.abort();
    loop_result
}

/// Small helper used by the CLI to format a peer list.
pub fn describe_peers(peers: &[PeerInfo]) -> String {
    if peers.is_empty() {
        return "no machines online".to_string();
    }
    let mut out = String::new();
    for p in peers {
        out.push_str(&format!(
            "{:<22} {:<18} {:<14} v{}\n",
            p.name,
            if p.hostname.is_empty() {
                "-"
            } else {
                &p.hostname
            },
            if p.os.is_empty() { "-" } else { &p.os },
            if p.version.is_empty() {
                "?"
            } else {
                &p.version
            }
        ));
    }
    out
}
