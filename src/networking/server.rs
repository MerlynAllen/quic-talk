use std::{
    ascii, fs,
    io::{self, Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    path::{self, Path, PathBuf},
    str,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};
use tracing_futures::Instrument as _;
use url::Url;

use crate::networking::common;
use crate::networking::session::{Session, SessionState};
use crate::QuicTalkState;
use crate::{Opt, SESSIONS, SHOULD_CLOSE};

pub(crate) async fn server(localhost: SocketAddr, options: Opt) -> Result<JoinHandle<Result<()>>> {
    let (certs, key) = if let (Some(key_path), Some(cert_path)) = (&options.key, &options.cert) {
        debug!("Loading private key.");
        let key = fs::read(key_path).context("failed to read private key")?;
        let key = if key_path.extension().map_or(false, |x| x == "der") {
            debug!("Key Format DER");
            rustls::PrivateKey(key)
        } else {
            let pkcs8 = rustls_pemfile::pkcs8_private_keys(&mut &*key)
                .context("malformed PKCS #8 private key")?;
            match pkcs8.into_iter().next() {
                Some(x) => {
                    debug!("Key format pkcs8");
                    rustls::PrivateKey(x)
                }
                None => {
                    let rsa = rustls_pemfile::rsa_private_keys(&mut &*key)
                        .context("malformed PKCS #1 private key")?;
                    match rsa.into_iter().next() {
                        Some(x) => {
                            debug!("Key format pkcs1");
                            rustls::PrivateKey(x)
                        }
                        None => {
                            bail!("no private keys found");
                        }
                    }
                }
            }
        };
        debug!("Loading cert chain.");
        let cert_chain = fs::read(cert_path).context("failed to read certificate chain")?;
        let cert_chain = if cert_path.extension().map_or(false, |x| x == "der") {
            vec![rustls::Certificate(cert_chain)]
        } else {
            rustls_pemfile::certs(&mut &*cert_chain)
                .context("invalid PEM-encoded certificate")?
                .into_iter()
                .map(rustls::Certificate)
                .collect()
        };

        (cert_chain, key)
    } else {
        debug!("Cert not found from given path, trying default path.");
        let dirs = directories_next::ProjectDirs::from("dev", "merlyn", "quic-talk").unwrap();
        let path = dirs.data_local_dir();
        let cert_path = path.join("cert.der");
        let key_path = path.join("key.der");
        let (cert, key) = match fs::read(&cert_path).and_then(|x| Ok((x, fs::read(&key_path)?))) {
            Ok(x) => {
                debug!("Found.");
                x
            }
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                debug!("Generating self-signed certificate");
                let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
                let key = cert.serialize_private_key_der();
                let cert = cert.serialize_der().unwrap();
                fs::create_dir_all(path).context("failed to create certificate directory")?;
                fs::write(&cert_path, &cert).context("failed to write certificate")?;
                fs::write(&key_path, &key).context("failed to write private key")?;
                debug!("Written to {key_path:?} and {cert_path:?}.");
                (cert, key)
            }
            Err(e) => {
                debug!("Cannot read certificate.");
                bail!("failed to read certificate: {}", e);
            }
        };
        debug!("Done");
        let key = rustls::PrivateKey(key);
        let cert = rustls::Certificate(cert);
        (vec![cert], key)
    };
    debug!("Configuring server.");
    let mut server_crypto = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key);
    let mut server_crypto = match server_crypto {
        Ok(x) => x,
        Err(e) => {
            panic!("Critical error: {e:?}");
        },
    };

    server_crypto.alpn_protocols = common::ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    if options.keylog {
        server_crypto.key_log = Arc::new(rustls::KeyLogFile::new());
    }

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(0_u8.into());
    // TODO - Set max_idle_timeout to 0 to disable idle timeout JUST FOT TESTING
    transport_config.max_idle_timeout(None);
    //    #[cfg(any(windows, os = "linux"))]
    //    transport_config.mtu_discovery_config(Some(quinn::MtuDiscoveryConfig::default()));
    if options.stateless_retry {
        server_config.use_retry(true);
    }
    //    let root = Arc::<Path>::from(options.root.clone());
    //    if !root.exists() {
    //        bail!("root path does not exist");
    //    }
    debug!("Listening");
    let endpoint = quinn::Endpoint::server(server_config, localhost)?;
    info!("Listening on {}", endpoint.local_addr()?);
    let task: JoinHandle<Result<()>> = tokio::spawn(async move {
        while let Some(connecting) = endpoint.accept().await {
            // Accept connections from different remote socket
            debug!("New connection accepted");
            let connection = connecting.await?;
            info!(
                "Connection incoming from {remote:?}",
                remote = connection.remote_address()
            );
            let session = match Session::new(connection).await {
                Ok(x) => x,
                Err(e) => {
                    error!("Failed to create session: {e:?}");
                    continue;
                },
            };
            let mut sessions_list = SESSIONS.write().await;
            sessions_list.push(session);
            debug!(
                "New session created. Currently {num} active sessions.",
                num = sessions_list.len()
            );
        }
        Ok(())
    });

    Ok(task)
}
