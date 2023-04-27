// crate import
mod networking;
mod state;
mod ui;
mod message;


use message::Message;
use state::QuicTalkState;

// external imports
use clap::Parser;
use std::{
    ascii, fs,
    io::{self},
    net::{SocketAddr, ToSocketAddrs},
    path::{self, Path, PathBuf},
    str,
    sync::Arc,
};
use std::io::Write;
use std::process::exit;
use std::sync::Condvar;

use anyhow::{anyhow, bail, Context, Result};
use tracing::{debug, error, info, info_span, Level, trace};
use tracing_futures::Instrument as _;
use url::Url;
use lazy_static::lazy_static;
use networking::{session::Session};
use tokio::sync::{RwLock, Mutex};
use std::sync::atomic::{AtomicBool};
use tokio::task::JoinHandle;
use signal_hook::consts::{SIGINT};
use crate::networking::{connect, server};
use crate::networking::session::SessionState;


#[derive(Parser, Debug, Clone)]
#[command(name = "quic-talk", version = "0.1.0", about = "A simple QUIC chat application")]
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

//Global Variables
lazy_static! {
    static ref SESSIONS: Arc<RwLock<Vec<Arc<Session>>>> = Arc::new(RwLock::new(Vec::new()));
    static ref MESSAGES: Arc<RwLock<Vec<Message>>> = Arc::new(RwLock::new(Vec::new()));
    static ref SHOULD_CLOSE: Arc<RwLock<bool>> = Arc::new(RwLock::new(false)); // Since this is a frequently accessed variable, we use a RwLock instead of a Mutex
}


#[tokio::main]
async fn main() -> Result<()> {
    // SIGINT handler
    let mut signals = signal_hook::iterator::Signals::new(&[SIGINT])?;
    let should_close = SHOULD_CLOSE.clone();
    tokio::spawn(async move {
        let mut ctrl_c_count = 0usize;
        for _ in signals.forever() {
            ctrl_c_count += 1;
            if ctrl_c_count >= 2 {
                break;
            }
            let mut should_close = should_close.write().await;
            *should_close = true;
            info!("Caught SIGINT, closing...");

        }
        info!("Force exiting...");
        exit(1);
    });

    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            // .with_max_level(Level::DEBUG)
            .finish(),
    )
        .unwrap();
    let opt = Opt::parse();

    // create a server task
    // TODO check config for server
    let server = server(opt.listen, opt.clone()).await?;


    // TODO currently read target from config
    let (hostname, remote) = match resolve_url(opt.remote.clone()) {
        Ok(target) => target,
        Err(e) => {
            error!("Failed to resolve target: {}", e);
            bail!("invalid URL");
        }
    };


    let dispatcher: JoinHandle<Result<()>> = tokio::spawn(async move {
        loop {
            // TODO check config for client
            if *SHOULD_CLOSE.clone().read().await {
                break;
            }
            for message in MESSAGES.write().await.drain(..) {
                let message = message.as_string().ok_or(
                    anyhow!("invalid message bytes")
                )?;
                println!(">>{}", message);
            }

        }
        Ok(())
    });


    let console: JoinHandle<Result<()>> = tokio::spawn(async move {
        debug!("Starting console");
        loop {
            // TODO check config for client
            if *SHOULD_CLOSE.clone().read().await {
                break;
            }
            let mut input = String::new();
            print!("<<");
            io::stdout().flush().unwrap();
            io::stdin().read_line(&mut input).unwrap();
            input.strip_suffix("\n").unwrap_or(&input);
            let message = Message::from_string(input);
            //你他妈倒是走网络啊
            if SESSIONS.read().await.len() == 0 {
                debug!("No session available, pushing message to queue");
                continue;
            } else {
                debug!("Session available, pushing message to session");
                SESSIONS.read().await[0].send_message(message).await?;
            }
            debug!("Message pushed to queue");
            debug!("Message queue length: {}", MESSAGES.read().await.len());
        }
        Ok(())
    });

    let client = tokio::spawn(connect(hostname, remote.port(), opt.clone()));

    server.await??;
    client.await??;
    dispatcher.await??;
    console.await??;
    Ok(())
}

fn resolve_url(url: String) -> Result<(String, SocketAddr)> {
    let url = match Url::parse(&url) {
        Ok(u) => u,
        Err(_) => bail!("cannot parse URL."),
    };
    // Check scheme
    if url.scheme() != "https" {
        bail!("URL Scheme can only be \"https\".")
    }
    let host = match url.host_str() {
        Some(h) => h,
        None => bail!("invalid URL"),
    };
    let port = match url.port() {
        Some(p) => p,
        None => networking::QUIC_DEFAULT_PORT,
    };
    // Remote socket
    let remote = format!("{host}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("couldn't resolve to an address"))?;

    Ok((host.to_string(), remote))
}

#[cfg(test)]
mod test {
    #[test]
    fn main() {
        println!("Hello, world!");
    }

    #[tokio::test]
    async fn stream_test() {}
}