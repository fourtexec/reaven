//! A deliberately tiny HTTP/1.1 server for the local live-viewer.
//!
//! Why hand-rolled instead of axum/hyper: the relay binary is intentionally
//! dependency-light, the surface is four routes, and streaming MJPEG is just a
//! long-lived response body. No TLS here -- this binds to loopback by default;
//! tunnel over SSH if you expose it further.
//!
//! Routes:
//!   GET  /              viewer page (inline CSS/JS, no external assets)
//!   GET  /stream.mjpg   multipart JPEG stream
//!   GET  /snapshot.jpg  most recent frame
//!   POST /input         JSON array of InputEvent -> forwarded to the agent
//!   GET  /health        "ok"

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};

use crate::protocol::{Frame, InputEvent};

const VIEWER_HTML: &str = include_str!("viewer.html");
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub struct ViewerConfig {
    pub bind: String,
    pub title: String,
    pub target: String,
    pub stream_id: String,
    pub frames: broadcast::Sender<Arc<Vec<u8>>>,
    pub latest: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
    pub outbound: mpsc::UnboundedSender<Frame>,
    pub allow_input: bool,
    /// Incremented by the HTTP layer so the CLI can report viewer activity.
    pub input_events: Arc<AtomicU64>,
}

pub async fn serve(cfg: ViewerConfig) -> Result<()> {
    let listener = TcpListener::bind(&cfg.bind)
        .await
        .with_context(|| format!("binding viewer on {}", cfg.bind))?;
    let cfg = Arc::new(cfg);
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, cfg.clone()).await {
                // Browser disconnects are normal; only log oddities.
                let msg = format!("{e:#}");
                if !msg.contains("connection reset") && !msg.contains("broken pipe") {
                    eprintln!("viewer {peer}: {msg}");
                }
            }
        });
    }
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut BufReader<TcpStream>) -> Result<Option<Request>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    // Read byte-by-byte until the blank line: simple and bounded.
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(None);
        }
        head.push(byte[0]);
        if head.len() >= 4 && head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > MAX_HEADER_BYTES {
            anyhow::bail!("request head too large");
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("body too large");
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).await?;
    }
    Ok(Some(Request { method, path, body }))
}

async fn handle(socket: TcpStream, cfg: Arc<ViewerConfig>) -> Result<()> {
    let mut socket = BufReader::new(socket);
    let Some(request) = read_request(&mut socket).await? else {
        return Ok(());
    };
    let mut out = socket.into_inner();
    let path = request.path.split('?').next().unwrap_or("/").to_string();

    match (request.method.as_str(), path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let html = VIEWER_HTML
                .replace("__TITLE__", &html_escape(&cfg.title))
                .replace("__TARGET__", &html_escape(&cfg.target))
                .replace(
                    "__INPUT_MODE__",
                    if cfg.allow_input { "control" } else { "view" },
                );
            respond(&mut out, 200, "text/html; charset=utf-8", html.as_bytes()).await
        }
        ("GET", "/health") => respond(&mut out, 200, "text/plain", b"ok").await,
        ("GET", "/snapshot.jpg") => {
            let frame = cfg.latest.lock().unwrap().clone();
            match frame {
                Some(jpeg) => respond(&mut out, 200, "image/jpeg", &jpeg).await,
                None => respond(&mut out, 503, "text/plain", b"no frame yet").await,
            }
        }
        ("GET", "/stream.mjpg") => stream_mjpeg(out, cfg.clone()).await,
        ("POST", "/input") => {
            if !cfg.allow_input {
                return respond(&mut out, 403, "text/plain", b"input disabled").await;
            }
            match parse_events(&request.body) {
                Ok(events) if events.is_empty() => {
                    respond(
                        &mut out,
                        200,
                        "application/json",
                        br#"{"ok":true,"applied":0}"#,
                    )
                    .await
                }
                Ok(events) => {
                    let count = events.len();
                    let frame = Frame::Control {
                        id: uuid::Uuid::new_v4().to_string(),
                        from: String::new(),
                        target: cfg.target.clone(),
                        stream: Some(cfg.stream_id.clone()),
                        events,
                    };
                    if cfg.outbound.send(frame).is_err() {
                        return respond(&mut out, 503, "text/plain", b"link closed").await;
                    }
                    cfg.input_events.fetch_add(count as u64, Ordering::Relaxed);
                    let body = format!(r#"{{"ok":true,"applied":{count}}}"#);
                    respond(&mut out, 200, "application/json", body.as_bytes()).await
                }
                Err(e) => {
                    let body = format!(r#"{{"ok":false,"error":"{}"}}"#, e);
                    respond(&mut out, 400, "application/json", body.as_bytes()).await
                }
            }
        }
        _ => respond(&mut out, 404, "text/plain", b"not found").await,
    }
}

fn parse_events(body: &[u8]) -> std::result::Result<Vec<InputEvent>, String> {
    serde_json::from_slice::<Vec<InputEvent>>(body).map_err(|e| e.to_string().replace('"', "'"))
}

async fn respond(out: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    );
    out.write_all(head.as_bytes()).await?;
    out.write_all(body).await?;
    out.flush().await?;
    Ok(())
}

async fn stream_mjpeg(mut out: TcpStream, cfg: Arc<ViewerConfig>) -> Result<()> {
    let mut rx = cfg.frames.subscribe();
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: multipart/x-mixed-replace; boundary=reavenframe\r\n\
                Cache-Control: no-store\r\n\
                Connection: close\r\n\
                Access-Control-Allow-Origin: *\r\n\r\n";
    out.write_all(head.as_bytes()).await?;

    // Send the current frame immediately so the page is not blank on connect.
    // (Take the lock in its own statement: a guard held across `.await` would
    // make this future non-Send.)
    let current = cfg.latest.lock().unwrap().clone();
    if let Some(jpeg) = current {
        write_part(&mut out, &jpeg).await?;
    }
    loop {
        match rx.recv().await {
            Ok(jpeg) => write_part(&mut out, &jpeg).await?,
            // A slow client skipped frames: fine, just keep going.
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

async fn write_part(out: &mut TcpStream, jpeg: &[u8]) -> Result<()> {
    let header = format!(
        "--reavenframe\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    );
    out.write_all(header.as_bytes()).await?;
    out.write_all(jpeg).await?;
    out.write_all(b"\r\n").await?;
    out.flush().await?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_input_events() {
        let body = br#"[{"event":"click","button":"left","x":0.5,"y":0.25}]"#;
        let events = parse_events(body).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], InputEvent::Click { .. }));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_events(b"not json").is_err());
    }

    #[test]
    fn escapes_html() {
        assert_eq!(html_escape("<a&b>"), "&lt;a&amp;b&gt;");
    }
}
