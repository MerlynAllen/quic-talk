// crate import
mod networking;
mod state;
mod ui;
mod message;
mod user;


// external imports
use clap::Parser;
use std::{
    ascii, fs,
    net::{SocketAddr, ToSocketAddrs},
    path::{self, Path, PathBuf},
    str,
    sync::Arc,
    io::Write,
    process::exit,
    sync::Condvar,
    panic::{self, PanicInfo},
    backtrace::Backtrace,
};

use anyhow::{anyhow, bail, Context, Result};
use tracing::{debug, error, info, info_span, Level, trace};
use tracing_futures::Instrument as _;
use url::Url;
use lazy_static::lazy_static;
use signal_hook::consts::{SIGINT};

use tokio::{
    sync::{RwLock, Mutex},
    task::JoinHandle,
    io::{self, AsyncWriteExt},
    time,
};
use url::quirks::username;
use crate::{
    networking::{
        connect,
        server,
        session::{
            Session,
            SessionState,
        },
    },
    message::Message,
    state::QuicTalkState,
    user::User,
    ui::UI,
};
use crate::message::Body;

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
    #[arg(long, default_value = "[::1]:12345")]
    listen: SocketAddr,

    /// Mode (Server / Client)
    #[arg(long, default_value = "server")]
    mode: String,

    /// Address to connect, in URL form (Client only)
    #[arg(long, default_value = "https://localhost:12345")]
    remote: String,

    /// Host name to overwrite (Client only), optional.
    #[arg(long)]
    hostname: Option<String>,

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
    static ref MESSAGES: Arc<RwLock<Vec<Message>>> = Arc::new(RwLock::new(Vec::new())); // Messages to be shown in the UI
    static ref SHOULD_CLOSE: Arc<RwLock<bool>> = Arc::new(RwLock::new(false)); // Since this is a frequently accessed variable, we use a RwLock instead of a Mutex
    // static ref STDIN_BUFFER: Arc<RwLock<Vec<u8>>> = Arc::new(RwLock::new(Vec::new()));
    static ref ME: RwLock<Option<User>> = RwLock::new(None);

}


#[tokio::main]
async fn main() -> Result<()> {
    // SIGINT handler
    let mut signals = signal_hook::iterator::Signals::new(&[SIGINT])?;
    let should_close = SHOULD_CLOSE.clone();
    // abort on panic
    panic::set_hook(Box::new(|info| {
        //let stacktrace = Backtrace::capture();
        let stacktrace = Backtrace::force_capture();
        println!("Got panic. @info:{}\n@stackTrace:{}", info, stacktrace);
        std::process::abort();
    }));

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
            debug!("Pressed {ctrl_c_count} times, SHOULD_CLOSE state: {should_close}");
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
    trace!("Starting");
    let opt = Opt::parse();

    // Setup test user
    // TODO just testing
    {
        let mut me = ME.write().await;
        if opt.mode == "client" {
            *me = Some(User::new("client".to_string(), "localhost@localhost".to_string(), "a7d399659b333e931046e4959e635e32".to_string()));
        } else {
            *me = Some(User::new("server".to_string(), "localhost@localhost".to_string(), "e8b32bc4d7b564ac6075a1418ad8841e".to_string()));
        }
    }


    // create a server task
    // TODO check config for server

    let server = if opt.mode == "server" {
        debug!("Server mode.");
        server(opt.listen, opt.clone()).await?
    } else {
        tokio::spawn(async move { Ok(()) })
    };


    // TODO currently read target from config
    let (hostname, remote) = match resolve_url(opt.remote.clone()) {
        Ok(target) => target,
        Err(e) => {
            error!("Failed to resolve target: {}", e);
            bail!("invalid URL");
        }
    };

    //
    // let console: JoinHandle<Result<()>> = tokio::spawn(async move {
    //     debug!("Starting console");
    //     loop {
    //         // TODO check config for client
    //         if *SHOULD_CLOSE.clone().read().await {
    //             break;
    //         }
    //         let mut input = String::new();
    //         print!("<<");
    //         io::stdout().flush().unwrap();
    //         io::stdin().read_line(&mut input).unwrap();
    //         input.strip_suffix("\n").unwrap_or(&input);
    //         let message = Message::from_string(input);
    //         if SESSIONS.read().await.len() == 0 {
    //             debug!("No session available, pushing message to queue");
    //             continue;
    //         } else {
    //             debug!("Session available, pushing message to session");
    //             SESSIONS.read().await[0].send_message(message).await?;
    //         }
    //         debug!("Message pushed to queue");
    //         debug!("Message queue length: {}", MESSAGES.read().await.len());
    //     }
    //     Ok(())
    // });

    let client = if opt.mode == "client" {
        tokio::spawn(connect(hostname, remote.port(), opt.clone()))
    } else {
        tokio::spawn(async move { Ok(()) })
    };

    // Garbage collector is not waited
    let _gc: JoinHandle<Result<()>> = tokio::spawn(async move {
        loop {
            let mut sessions = SESSIONS.write().await;
            let mut i = 0;
            while i < sessions.len() {
                if sessions[i].is_terminated().await {
                    debug!("Session with {:?} terminated, removing...", sessions[i].get_peername().await);
                    sessions.remove(i);
                } else {
                    i += 1;
                }
            }
            time::sleep(time::Duration::from_secs(1)).await;
        }
        Ok(())
    });
    // Do not load console unless session is ready
    while SESSIONS.read().await.len() > 0 {
        time::sleep(time::Duration::from_secs(1)).await;
    }
    let _console = UI::console()?;
    // Gather all tasks
    loop {
        let should_close = *SHOULD_CLOSE.clone().read().await;
        if should_close {
            if SESSIONS.read().await.len() == 0 {
                debug!("No session available, closing...");
                exit(0); // end without waiting if no session is available
            } else {
                debug!("Session available, waiting for session to close...");
                for session in SESSIONS.write().await.iter_mut() {
                    // Close all sessions
                    session.close().await;
                }
                break;
            }
        }
    }
    debug!("All tasks finished, exiting...");
    exit(0);
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
