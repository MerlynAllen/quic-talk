use std::{
    fs,
    io::{self, Write},
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc},
    time::{Duration, Instant},
};
use tokio::sync::{RwLock, Mutex};
use anyhow::{anyhow, bail, Result};
use clap::Parser;
use tracing::{debug, error, info};

use crate::networking::common;
use crate::networking::session::{Session, SessionState};
use crate::Opt;
use crate::QuicTalkState;

pub(crate) async fn client(
    hostname: String,
    port: u16,
    options: Opt,
    global_states: Arc<QuicTalkState>,
) -> Result<()> {
    let remote = format!("{hostname}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or(anyhow!("invalid hostname or port"))?;
    // Read trusted CA cert
    // (Able to add multiple CA certs)
    let mut roots = rustls::RootCertStore::empty();
    if let Some(ca_path) = options.trusted_ca {
        roots.add(&rustls::Certificate(fs::read(ca_path)?))?;
    } else {
        // Get certs from program data dir.
        let dirs = directories_next::ProjectDirs::from("dev", "merlyn", "quic-talk").unwrap();
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
    let hostname = options
        .host
        .as_ref()
        .map_or_else(|| Some(hostname), |x| Some(x.to_string()))
        .ok_or_else(|| anyhow!("no hostname specified"))?;

    info!("Connecting to {hostname} at {remote:?}");
    // Create a connection
    let conn = endpoint
        .connect(remote, hostname.as_str())?
        .await
        .map_err(|e| anyhow!("failed to connect: {}", e))?;
    info!("Connected at {:?}", start.elapsed());
    // Add session to global sessions list
    let session = Session {
        role: SessionRole::Client,
        state: Mutex::new(SessionState::Ready),
        conn,
        recv: RwLock::new(None),
        send: RwLock::new(None),
    };
    let sessions_list = global_states.sessions.clone();
    {
        let lock = sessions_list.write();
        match lock {
            Err(_) => {
                bail!("try locking global session list failed!")
            }
            Ok(mut l) => {
                (*l).push(Arc::new(session));
                debug!(
                    "New session created. Currently {num} active sessions.",
                    num = (*l).len()
                );
            }
        }
    }
    Ok(())
}
//
//fn duration_secs(x: &Duration) -> f32 {
//    x.as_secs() as f32 + x.subsec_nanos() as f32 * 1e-9
//}
