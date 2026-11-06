//! TLS front door for the relay.
//!
//! Three modes, in priority order:
//!
//! * **ACME** (`--acme-domain`): obtain and renew a Let's Encrypt certificate
//!   automatically using TLS-ALPN-01. No HTTP challenge server is needed, and
//!   the certificate is served on the same port as the relay. Port 443 is
//!   required for Let's Encrypt validation.
//! * **Files** (`--tls-cert` + `--tls-key`): use an existing PEM certificate.
//! * **Self-signed** (`--self-signed`): generate a throwaway certificate for
//!   local testing. Clients must use `--insecure`.
//!
//! All accepted TLS streams are handed to the relay through one channel, so the
//! rest of the program does not care which certificate backend is in use.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use rustls_acme::caches::DirCache;
use rustls_acme::AcmeConfig;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_stream::wrappers::TcpListenerStream;

use crate::transport::BoxedIo;

#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    pub acme_domains: Vec<String>,
    pub acme_email: Option<String>,
    pub acme_cache: PathBuf,
    pub acme_staging: bool,
    pub cert: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub self_signed: bool,
}

/// Bind the listening socket now (so address errors surface at startup), then
/// asynchronously accept connections and run the TLS handshake off the accept
/// path. Completed streams arrive on the returned channel.
pub async fn bind(listen: &str, options: TlsOptions) -> Result<mpsc::Receiver<BoxedIo>> {
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    let local = listener.local_addr()?;
    let (tx, rx) = mpsc::channel::<BoxedIo>(64);

    if !options.acme_domains.is_empty() {
        start_acme(listener, local.port(), options, tx);
    } else {
        let acceptor = build_acceptor(&options)?;
        start_tls(listener, acceptor, tx);
    }
    Ok(rx)
}

fn start_acme(listener: TcpListener, port: u16, options: TlsOptions, tx: mpsc::Sender<BoxedIo>) {
    let domains = options.acme_domains;
    let cache = options.acme_cache;
    let contact = options
        .acme_email
        .iter()
        .map(|email| {
            if email.starts_with("mailto:") {
                email.clone()
            } else {
                format!("mailto:{email}")
            }
        })
        .collect::<Vec<_>>();

    println!(
        "TLS: automatic Let's Encrypt certificate for {} ({})",
        domains.join(", "),
        if options.acme_staging {
            "staging"
        } else {
            "production"
        }
    );
    println!("TLS: certificate cache {}", cache.display());
    if port != 443 {
        eprintln!(
            "WARNING: ACME TLS-ALPN-01 validation uses port 443; this relay listens on {port}. \
             Forward external port 443 to it, or validation will fail."
        );
    }

    tokio::spawn(async move {
        let mut incoming = AcmeConfig::new(domains)
            .contact(contact)
            .directory_lets_encrypt(!options.acme_staging)
            .cache(DirCache::new(cache))
            .tokio_incoming(TcpListenerStream::new(listener), vec![b"http/1.1".to_vec()]);

        while let Some(item) = incoming.next().await {
            match item {
                Ok(tls) => {
                    if tx.send(Box::new(tls)).await.is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("TLS accept error: {e:#}"),
            }
        }
    });
}

fn start_tls(listener: TcpListener, acceptor: TlsAcceptor, tx: mpsc::Sender<BoxedIo>) {
    tokio::spawn(async move {
        loop {
            let (tcp, remote) = match listener.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("TLS accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let _ = tcp.set_nodelay(true);
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                match acceptor.accept(tcp).await {
                    Ok(tls) => {
                        let _ = tx.send(Box::new(tls)).await;
                    }
                    Err(e) => eprintln!("{remote}: TLS handshake failed: {e}"),
                }
            });
        }
    });
}

fn build_acceptor(options: &TlsOptions) -> Result<TlsAcceptor> {
    let (certs, key) = match (&options.cert, &options.key) {
        (Some(cert), Some(key)) => {
            println!(
                "TLS: using certificate {} and key {}",
                cert.display(),
                key.display()
            );
            (load_certs(cert)?, load_key(key)?)
        }
        (Some(_), None) => bail!("--tls-cert was given without --tls-key"),
        (None, Some(_)) => bail!("--tls-key was given without --tls-cert"),
        (None, None) if options.self_signed => {
            eprintln!(
                "WARNING: using a throwaway self-signed certificate. \
                 Clients must pass --insecure. Do not use this for a public relay."
            );
            self_signed()
        }
        (None, None) => bail!(
            "no TLS certificate configured. Use --acme-domain <domain> [--acme-email <email>] \
             for automatic Let's Encrypt, --tls-cert/--tls-key for an existing certificate, \
             or --self-signed for local testing."
        ),
    };

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("building TLS server configuration")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("loading TLS certificate and key")?;
    // Advertise HTTP/1.1 for ordinary HTTPS clients; WSS needs no ALPN, but
    // this keeps `curl https://host/` from negotiating a protocol we do not
    // speak.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

fn self_signed() -> (
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let generated = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ])
    .expect("generating a self-signed certificate");
    let cert = generated.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(generated.key_pair.serialize_der());
    (vec![cert], key.into())
}

fn load_certs(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening certificate {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("parsing certificate {}", path.display()))?;
    if certs.is_empty() {
        bail!("certificate {} contains no certificates", path.display());
    }
    Ok(certs)
}

fn load_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("opening key {}", path.display()))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("parsing key {}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("key {} contains no private key", path.display()))
}

/// Default certificate cache location: `$HOME/.reaven/acme`, or a local
/// directory when HOME is unset (e.g. some service managers).
pub fn default_acme_cache() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".reaven")
        .join("acme")
}
