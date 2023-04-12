mod networking;
mod state;
use clap::Parser;
use std::{
    ascii, fs, io,
    net::SocketAddr,
    path::{self, Path, PathBuf},
    str,
    sync::Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use tracing::{error, info, info_span, Level};
use tracing_futures::Instrument as _;

use state::QuicTalkState;



#[derive(Parser, Debug)]
#[command(name = "server")]
struct Opt {
    /// file to log TLS keys to for debugging
    #[arg(long)]
    keylog: bool,

    /// TLS private key in PEM format
    #[arg(short, long, requires = "cert")]
    key: Option<PathBuf>,

    /// TLS certificate in PEM format
    #[arg(short, long, requires = "key")]
    cert: Option<PathBuf>,

    /// Enable stateless retries
    #[arg(long)]
    stateless_retry: bool,

    /// Address to listen on (Server only)
    #[arg(long, default_value = "[::1]:4433")]
    listen: SocketAddr,

    /// Mode (Server / Client)
    #[arg(long, default_value = "server")]
    mode: String,

    /// Address to connect, in URL form (Client only)
    #[arg(long, default_value = "[::1]::4433")]
    remote: String,

    /// Host name to overwrite (Client only), optional.
    #[arg(long)]
    host: Option<String>,

    /// Simulate NAT rebinding after connecting
    #[arg(long)]
    rebind: bool,

    /// Custom certificate authority to trust, in DER format
    #[arg(long)]
    trusted_ca: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();
    let opt = Opt::parse();
    let global_state = QuicTalkState::new();
    // Temporarily write like this (for testing),
    // later write a new test module.
    let code = match opt.mode.as_str() {
        "server" => {
            if let Err(e) = networking::server(opt, global_state).await {
                eprintln!("ERROR: {e}");
                1
            } else {
                0
            }
        }
        "client" => {
            if let Err(e) = networking::client(opt, global_state).await {
                eprintln!("ERROR: {e}");
                1
            } else {
                0
            }
        }
        _ => {
            panic!("\"{}\" mode is not a valid mode!", opt.mode);
        }
    };
    ::std::process::exit(code);
}
