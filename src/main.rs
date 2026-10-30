//! Reaven — server-relayed remote administration.
//!
//! Subcommands fall into three groups:
//!   * relay:      `server`
//!   * agent:      `connect`, `install-autostart`
//!   * operator:   `peers`, `cl`, `exec`, `upload`, `download`, `screens`,
//!     `cameras`, `screenshot`, `screen`, `webcam`, `input`

mod agent;
mod desktop;
mod httpd;
mod operator;
mod protocol;
mod relay;

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};

use protocol::{MouseButton, Operation, VideoSource};

#[derive(Parser)]
#[command(
    name = "reaven",
    version,
    about = "Server-relayed remote administration: shell, files, screen, webcam, input",
    long_about = None
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
        /// Do not accept remote mouse/keyboard input on this machine.
        #[arg(long)]
        no_input: bool,
    },
    /// List the machines currently online.
    Peers {
        #[arg(long)]
        server: String,
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
        /// Install a view-only agent (no remote input).
        #[arg(long)]
        no_input: bool,
    },
    /// List the monitors of a peer.
    Screens {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// List the webcams of a peer.
    Cameras {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Grab a single screenshot from a peer.
    Screenshot {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        /// Where to write the JPEG.
        #[arg(long, default_value = "screenshot.jpg")]
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        #[arg(long, default_value_t = 85, value_parser = clap::value_parser!(u8).range(1..=100))]
        quality: u8,
        /// Down-scale so the long edge is at most this many pixels (0 = native).
        #[arg(long, default_value_t = 0)]
        max_width: u32,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Stream a peer's screen and serve a live viewer you can also control.
    Screen {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long, default_value_t = 0)]
        monitor: u32,
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u8).range(1..=60))]
        fps: u8,
        #[arg(long, default_value_t = 70, value_parser = clap::value_parser!(u8).range(1..=100))]
        quality: u8,
        #[arg(long, default_value_t = 1600)]
        max_width: u32,
        /// Address for the browser viewer.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// Do not start the browser viewer; frames are only recorded/reported.
        #[arg(long)]
        no_viewer: bool,
        /// Save every received frame as JPEG into this directory.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Stop automatically after N seconds.
        #[arg(long)]
        duration: Option<u64>,
        /// Start the viewer in view-only mode (no remote input).
        #[arg(long)]
        view_only: bool,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Stream a peer's webcam, or grab a single frame with --snapshot.
    Webcam {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        #[arg(long, default_value_t = 0)]
        index: u32,
        /// Write one JPEG and exit instead of streaming.
        #[arg(long)]
        snapshot: Option<PathBuf>,
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(1..=60))]
        fps: u8,
        #[arg(long, default_value_t = 75, value_parser = clap::value_parser!(u8).range(1..=100))]
        quality: u8,
        #[arg(long, default_value_t = 1280)]
        max_width: u32,
        #[arg(long, default_value = "127.0.0.1:8081")]
        bind: String,
        #[arg(long)]
        no_viewer: bool,
        #[arg(long)]
        record: Option<PathBuf>,
        #[arg(long)]
        duration: Option<u64>,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
    /// Inject mouse/keyboard input into a peer that is currently streaming.
    Input {
        #[arg(long)]
        server: String,
        #[arg(long)]
        target: String,
        /// Normalised position as X,Y in 0..1 (default 0.5,0.5).
        #[arg(long, value_parser = parse_point)]
        at: Option<(f32, f32)>,
        /// Click a button: left, middle or right.
        #[arg(long)]
        click: Option<String>,
        /// With --click, send a double click.
        #[arg(long)]
        double: bool,
        /// Vertical scroll steps (positive scrolls down).
        #[arg(long, allow_hyphen_values = true)]
        scroll: Option<i32>,
        /// Horizontal scroll steps.
        #[arg(long, allow_hyphen_values = true)]
        scroll_x: Option<i32>,
        /// Press and release one key, e.g. Return, F5, a.
        #[arg(long)]
        key: Option<String>,
        /// Type a string.
        #[arg(long)]
        text: Option<String>,
        /// Key chord, e.g. ctrl+alt+t.
        #[arg(long)]
        combo: Option<String>,
        #[arg(long, env = "REAVEN_PASSWORD")]
        password: Option<String>,
    },
}

fn parse_point(raw: &str) -> Result<(f32, f32)> {
    let (x, y) = raw
        .split_once(',')
        .ok_or_else(|| anyhow!("expected X,Y in 0..1, e.g. 0.5,0.5"))?;
    let x: f32 = x.trim().parse().context("parsing X")?;
    let y: f32 = y.trim().parse().context("parsing Y")?;
    if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        bail!("coordinates must be between 0 and 1");
    }
    Ok((x, y))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Server { listen, password } => relay::run_server(&listen, password).await,
        Commands::Connect {
            server,
            name,
            password,
            no_input,
        } => {
            agent::run_peer(
                &server,
                &name,
                password,
                agent::AgentOptions {
                    allow_input: !no_input,
                },
            )
            .await
        }
        Commands::Peers { server, password } => operator::run_peers(&server, password).await,
        Commands::Cl {
            server,
            target,
            password,
        } => operator::run_cl(&server, password, &target).await,
        Commands::Exec {
            server,
            target,
            command,
            password,
        } => operator::run_exec(&server, password, &target, command).await,
        Commands::Upload {
            server,
            target,
            local,
            remote,
            password,
        } => operator::run_upload(&server, password, &target, local, remote).await,
        Commands::Download {
            server,
            target,
            remote,
            local,
            password,
        } => operator::run_download(&server, password, &target, remote, local).await,
        Commands::InstallAutostart {
            server,
            name,
            password,
            no_input,
        } => install_autostart(&server, &name, password.as_deref(), no_input),
        Commands::Screens {
            server,
            target,
            password,
        } => operator::run_screens(&server, password, &target).await,
        Commands::Cameras {
            server,
            target,
            password,
        } => operator::run_cameras(&server, password, &target).await,
        Commands::Screenshot {
            server,
            target,
            output,
            monitor,
            quality,
            max_width,
            password,
        } => {
            operator::run_screenshot(
                &server, password, &target, monitor, quality, max_width, &output,
            )
            .await
        }
        Commands::Screen {
            server,
            target,
            monitor,
            fps,
            quality,
            max_width,
            bind,
            no_viewer,
            record,
            duration,
            view_only,
            password,
        } => {
            operator::run_stream(operator::StreamOptions {
                server,
                password,
                target,
                source: VideoSource::Screen { monitor },
                fps,
                quality,
                max_width,
                bind: if no_viewer { None } else { Some(bind) },
                record,
                duration,
                allow_input: !view_only,
            })
            .await
        }
        Commands::Webcam {
            server,
            target,
            index,
            snapshot,
            fps,
            quality,
            max_width,
            bind,
            no_viewer,
            record,
            duration,
            password,
        } => match snapshot {
            Some(output) => {
                operator::run_camera_snapshot(
                    &server, password, &target, index, max_width, quality, &output,
                )
                .await
            }
            None => {
                operator::run_stream(operator::StreamOptions {
                    server,
                    password,
                    target,
                    source: VideoSource::Camera { index },
                    fps,
                    quality,
                    max_width,
                    bind: if no_viewer { None } else { Some(bind) },
                    record,
                    duration,
                    // A webcam frame has no stable desktop geometry, so remote
                    // pointer input does not apply to it.
                    allow_input: false,
                })
                .await
            }
        },
        Commands::Input {
            server,
            target,
            at,
            click,
            double,
            scroll,
            scroll_x,
            key,
            text,
            combo,
            password,
        } => {
            let button = match click {
                Some(raw) => Some(MouseButton::parse(&raw)?),
                None => None,
            };
            let events = operator::build_input_events(
                at, button, double, scroll, scroll_x, key, text, combo,
            );
            operator::run_input(&server, password, &target, events).await
        }
    }
}

fn install_autostart(
    server: &str,
    name: &str,
    password: Option<&str>,
    no_input: bool,
) -> Result<()> {
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
        "{} connect --server {} --name {}{}",
        quote(&exe.to_string_lossy()),
        quote(server),
        quote(name),
        if no_input { " --no-input" } else { "" }
    );
    let env_prefix = password
        .map(|p| format!("REAVEN_PASSWORD={} ", quote(p)))
        .unwrap_or_default();
    let block = format!(
        "\n{BEGIN}\nif [[ $- == *i* ]] && command -v flock >/dev/null 2>&1; then\n  (flock -n \"$HOME/.reaven.lock\" env {env_prefix}{args} >> \"$HOME/.reaven.log\" 2>&1 &)\nfi\n{END}\n"
    );
    content.push_str(&block);
    std::fs::write(&path, content).with_context(|| format!("updating {}", path.display()))?;
    println!(
        "Installed idempotent Reaven autostart in {}",
        path.display()
    );
    println!("Note: the password, if supplied, is stored in plaintext in .bashrc.");
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: Operation) {}
