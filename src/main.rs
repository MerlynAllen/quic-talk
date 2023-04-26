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
use std::sync::Condvar;

use anyhow::{anyhow, bail, Context, Result};
use tracing::{debug, error, info, info_span, Level, trace};
use tracing_futures::Instrument as _;
use url::Url;
use lazy_static::lazy_static;
use crate::networking::session::Session;
use tokio::sync::{RwLock, Mutex};
use std::sync::atomic::{AtomicBool};
use signal_hook::consts::{SIGINT};

#[derive(Parser, Debug, Clone)]
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

//Global Variables
lazy_static! {
    static ref SESSIONS: Arc<RwLock<Vec<Session>>> = Arc::new(RwLock::new(Vec::new()));
    static ref MESSAGES: Arc<RwLock<Vec<Message>>> = Arc::new(RwLock::new(Vec::new()));
    static ref SHOULD_CLOSE: Arc<RwLock<bool>> = Arc::new(RwLock::new(false)); // Since this is a frequently accessed variable, we use a RwLock instead of a Mutex
}


#[tokio::main]
async fn main() -> Result<()> {
    // SIGINT handler
    let mut signals = signal_hook::iterator::Signals::new(&[SIGINT])?;
    let should_close = SHOULD_CLOSE.clone();
    tokio::spawn(async move {
        for _ in signals.forever() {
            let mut should_close = should_close.write().await;
            *should_close = true;
        }
    });

    tracing::subscriber::set_global_default(
        tracing_subscriber::FmtSubscriber::builder()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_max_level(Level::DEBUG)
            .finish(),
    )
    .unwrap();
    let opt = Opt::parse();
    let global_state = Arc::new(QuicTalkState::new());
    let (global_state_c, global_state_s) = (global_state.clone(), global_state.clone());
    // Temporarily write like this (for testing),
    // later write a new test module.
    //    let code = match opt.mode.as_str() {
    //        "server" => {
    //            if let Err(e) = networking::server(opt, global_state).await {
    //                eprintln!("ERROR: {e}");
    //                1
    //            } else {
    //                0
    //            }
    //        }
    //        "client" => {
    //            if let Err(e) = networking::client(opt, global_state).await {
    //                eprintln!("ERROR: {e}");
    //                1
    //            } else {
    //                0
    //            }
    //        }
    //        _ => {
    //            panic!("\"{}\" mode is not a valid mode!", opt.mode);
    //        }
    //    };

    match opt.mode.as_str() {
        "server" => {
            let localhost = opt.listen;
            let s = networking::server(localhost, opt.clone(), global_state.clone());
            let s_process = tokio::spawn(async move {
                match s.await {
                    Ok(()) => 0,
                    Err(e) => {
                        error!("Error occurred when executing server: {e}");
                        1
                    }
                }
            });
            //            s_process.await;
            let server = tokio::spawn(async move {
                let sessions = global_state_s.sessions.clone();
                while sessions.read().unwrap().len() == 0 {} // Block until session exists
                let client_stream = match sessions.write() {
                    Ok(v) => v[0].clone(),
                    Err(e) => {
                        bail!("get mutex lock failed");
                    }
                };
                client_stream.open().await?;
                debug!("Trying to read!");
                let (nbytes, buf) = client_stream.read().await?;
                println!("Client said:{}", String::from_utf8(Vec::from(buf))?);
                client_stream.terminate().await;
                Ok(())
            });
            server.await?
        }
        "client" => {
            let (hostname, remote) = resolve_url(opt.remote.to_string())?;
            let c = networking::client(hostname, remote.port(), opt.clone(), global_state.clone());
            let c_process = tokio::spawn(async move {
                match c.await {
                    Ok(()) => 0,
                    Err(e) => {
                        error!("Error occurred when executing client: {e}");
                        1
                    }
                }
            });
            c_process.await;
            let client = tokio::spawn(async move {
                let sessions = global_state_c.sessions.clone();
                let client_stream = match sessions.write() {
                    Ok(v) => v[0].clone(),
                    Err(e) => {
                        bail!("get mutex lock failed");
                    }
                };

                client_stream.open().await;
                debug!("Trying to write!");
                client_stream.write("Hello!".as_bytes()).await;

                debug!("Written");
                client_stream.terminate().await;
                Ok(())
            });
            client.await?
        }
        _ => {
            bail!("invalid mode name");
        }
    }
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
    async fn stream_test() {
        
    }
}