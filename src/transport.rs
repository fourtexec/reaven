//! WSS transport: secure WebSocket carrying one JSON frame per message.
//!
//! Reaven deliberately looks like ordinary HTTPS traffic: clients connect to
//! `wss://relay.example/ws` on 443 with a normal browser-style WebSocket
//! upgrade. The relay, agents and operator CLI all speak this transport. There
//! is no public plaintext listener; the relay can opt into a loopback-only
//! plaintext listener for a local TLS-terminating reverse proxy.
//!
//! Frame framing is unchanged from protocol v2: each WebSocket text message is
//! one JSON object. Splitting the socket into independent read/write halves lets
//! the relay and agent keep their existing concurrent reader/writer design.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    accept_async_with_config, connect_async_tls_with_config, Connector, MaybeTlsStream,
    WebSocketStream,
};

use crate::protocol::{Frame, MAX_FRAME_BYTES};

/// Object-safe I/O trait so the relay can box TLS streams from different
/// backends (ACME, manual certificate, self-signed) behind one type.
pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}

pub type BoxedIo = Box<dyn AsyncIo>;
pub type ClientIo = MaybeTlsStream<tokio::net::TcpStream>;

pub struct WsConnection<S: AsyncIo> {
    inner: WebSocketStream<S>,
}

/// Read half of a WSS connection.
pub struct FrameReader<S: AsyncIo> {
    inner: futures_util::stream::SplitStream<WebSocketStream<S>>,
}

/// Write half of a WSS connection.
pub struct FrameWriter<S: AsyncIo> {
    inner: futures_util::stream::SplitSink<WebSocketStream<S>, Message>,
}

fn ws_config() -> Option<WebSocketConfig> {
    // Screen frames can approach the 32 MB protocol limit, so the WebSocket
    // layer must not impose a smaller per-frame ceiling than the protocol does.
    Some(
        WebSocketConfig::default()
            .max_message_size(Some(MAX_FRAME_BYTES))
            .max_frame_size(Some(MAX_FRAME_BYTES)),
    )
}

impl<S: AsyncIo> WsConnection<S> {
    /// Split into halves that can live in separate tasks.
    pub fn split(self) -> (FrameReader<S>, FrameWriter<S>) {
        let (sink, stream) = self.inner.split();
        (FrameReader { inner: stream }, FrameWriter { inner: sink })
    }
}

/// Complete the server side of a WebSocket handshake over an already
/// established (TLS) stream.
pub async fn accept(stream: BoxedIo) -> Result<WsConnection<BoxedIo>> {
    let inner = accept_async_with_config(stream, ws_config())
        .await
        .context("WebSocket handshake failed")?;
    Ok(WsConnection { inner })
}

/// Connect to a relay as a WebSocket client.
///
/// `server` accepts either a full URL (`wss://relay.example/ws`,
/// `https://relay.example`) or the older `host[:port]` form, which is upgraded
/// to `wss://host[:port]/ws`.
pub async fn connect(server: &str, insecure: bool) -> Result<WsConnection<ClientIo>> {
    let url = normalize_url(server)?;
    let is_secure = url.starts_with("wss://") || url.starts_with("https://");

    let connector = if is_secure {
        Some(Connector::Rustls(client_tls_config(insecure)?))
    } else {
        None
    };

    let (ws, response) = connect_async_tls_with_config(url.clone(), ws_config(), true, connector)
        .await
        .with_context(|| {
            let hint = if insecure {
                ""
            } else {
                " (if the relay uses a self-signed certificate, pass --insecure)"
            };
            format!("connecting to {url}{hint}")
        })?;
    if response.status().is_client_error() || response.status().is_server_error() {
        bail!(
            "relay rejected WebSocket upgrade: HTTP {}",
            response.status()
        );
    }
    Ok(WsConnection { inner: ws })
}

fn normalize_url(server: &str) -> Result<String> {
    let server = server.trim();
    if server.is_empty() {
        bail!("empty relay address");
    }

    let (mut url, had_path) = if let Some(rest) = server.strip_prefix("https://") {
        (format!("wss://{rest}"), rest.contains('/'))
    } else if let Some(rest) = server.strip_prefix("http://") {
        (format!("ws://{rest}"), rest.contains('/'))
    } else if server.starts_with("wss://") || server.starts_with("ws://") {
        (
            server.to_owned(),
            server.contains('/') && server.splitn(4, '/').nth(3).is_some(),
        )
    } else if server.contains("://") {
        bail!("unsupported relay scheme in '{server}' (use wss:// or https://)");
    } else {
        (format!("wss://{server}"), false)
    };

    // Add the conventional endpoint if the user only supplied a host. The
    // server accepts any path, but a real path makes the traffic look less
    // distinctive and keeps future HTTP routes possible.
    if !had_path {
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("ws");
    }
    Ok(url)
}

fn client_tls_config(insecure: bool) -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("building TLS client configuration")?;

    if insecure {
        return Ok(Arc::new(
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoCertificateVerification { provider }))
                .with_no_client_auth(),
        ));
    }

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(Arc::new(
        builder.with_root_certificates(roots).with_no_client_auth(),
    ))
}

/// Only used by `--insecure`, for local/self-signed relays. It still performs
/// the TLS handshake and signature operations, but skips chain and hostname
/// validation.
#[derive(Debug)]
struct NoCertificateVerification {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl<S: AsyncIo> FrameReader<S> {
    /// Read the next protocol frame. Ping/pong control messages are handled by
    /// tungstenite and skipped here.
    pub async fn recv(&mut self) -> Result<Frame> {
        loop {
            let message = self
                .inner
                .next()
                .await
                .ok_or_else(|| anyhow!("connection closed"))?
                .context("reading WebSocket message")?;
            match message {
                Message::Text(text) => {
                    if text.len() > MAX_FRAME_BYTES {
                        bail!("protocol frame exceeded {MAX_FRAME_BYTES} bytes");
                    }
                    return serde_json::from_str(text.as_str()).context("invalid protocol frame");
                }
                Message::Binary(bytes) => {
                    if bytes.len() > MAX_FRAME_BYTES {
                        bail!("protocol frame exceeded {MAX_FRAME_BYTES} bytes");
                    }
                    return serde_json::from_slice(&bytes).context("invalid protocol frame");
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => bail!("connection closed"),
                Message::Frame(_) => continue,
            }
        }
    }
}

impl<S: AsyncIo> FrameWriter<S> {
    /// Send one frame as one WebSocket text message.
    pub async fn send(&mut self, frame: &Frame) -> Result<()> {
        let json = serde_json::to_string(frame)?;
        self.inner
            .send(Message::text(json))
            .await
            .context("writing WebSocket message")
    }

    /// Send a WebSocket control ping. The remote WebSocket implementation
    /// automatically responds with a pong, keeping idle proxy connections alive.
    pub async fn ping(&mut self) -> Result<()> {
        self.inner
            .send(Message::Ping(Default::default()))
            .await
            .context("writing WebSocket ping")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_host_port_to_wss() {
        assert_eq!(
            normalize_url("relay.example:443").unwrap(),
            "wss://relay.example:443/ws"
        );
        assert_eq!(
            normalize_url("127.0.0.1:7331").unwrap(),
            "wss://127.0.0.1:7331/ws"
        );
        assert_eq!(
            normalize_url("wss://relay.example/custom").unwrap(),
            "wss://relay.example/custom"
        );
        assert_eq!(
            normalize_url("https://relay.example").unwrap(),
            "wss://relay.example/ws"
        );
        assert_eq!(
            normalize_url("ws://127.0.0.1:9000").unwrap(),
            "ws://127.0.0.1:9000/ws"
        );
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(normalize_url("ssh://relay.example").is_err());
    }
}
