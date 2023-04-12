use std::{
    fs,
    io::{self, Write},
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use tracing::{error, info, debug};
use url::Url;

use crate::networking::common;
use crate::Opt;
use crate::QuicTalkState;
use crate::networking::session::{QuicTalkSession, QuicTalkSessionState};
const QUIC_DEFAULT_PORT: u16 = 4433;



pub(crate) async fn client(options: Opt, global_states: QuicTalkState) -> Result<()> {
    let url = match Url::parse(&options.remote) {
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
        None => QUIC_DEFAULT_PORT,
    };
    // Remote socket
    let remote = format!("{host}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("couldn't resolve to an address"))?;

    // Read trusted CA cert
    // (Able to add multiple CA certs)
    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca_path) = options.trusted_ca {
        roots.add(&rustls::Certificate(fs::read(ca_path)?))?;
    } else {
        // Get certs from program data dir.
        let dirs = directories_next::ProjectDirs::from("org", "quinn", "quinn-examples").unwrap();
        match fs::read(dirs.data_local_dir().join("cert.der")) {
            Ok(cert) => {
                roots.add(&rustls::Certificate(cert))?;
            }
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                error!("Local server certificate not found!");
            }
            Err(e) => {
                error!("Failed to open local server certificate: {e}!");
            }
        }
    }
    // Create a new client config.
    let mut client_crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    // Set up connection
    client_crypto.alpn_protocols = common::ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    if options.keylog {
        client_crypto.key_log = Arc::new(rustls::KeyLogFile::new());
    }

    let mut client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
    common::enable_mtud_if_supported(&mut client_config);

    let mut endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())?; // Local listening port
    endpoint.set_default_client_config(client_config);

    let start = Instant::now();
    let rebind = options.rebind;
    // Remote host name overwrite
    let host = options
        .host
        .as_ref()
        .map_or_else(|| Some(host), |x| Some(x))
        .ok_or_else(|| anyhow!("no hostname specified"))?;

    info!("Connecting to {host} at {remote}");
    // Create a connection
    let conn = endpoint
        .connect(remote, host)?
        .await
        .map_err(|e| anyhow!("failed to connect: {}", e))?;
    info!("Connected at {:?}", start.elapsed());
    // Add session to global sessions list
    let session = QuicTalkSession {
        state: QuicTalkSessionState::Handshaking,
        conn,
        recv: None,
        send: None,
    };
    let sessions_list = global_states.sessions.clone();
    {

        let lock = sessions_list.lock();
        match lock {
            Err(_) => {
                bail!("try locking global session list failed!")
            },
            Ok(mut l) => {
                (*l).push(session);
                debug!("New session created. Currently {num} active sessions.", num = (*l).len() );
            }
        }
    }
    Ok(())
}
//
//fn duration_secs(x: &Duration) -> f32 {
//    x.as_secs() as f32 + x.subsec_nanos() as f32 * 1e-9
//}
