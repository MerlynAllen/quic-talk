use anyhow::{anyhow, bail, Result};
use quinn::{Connection, RecvStream, SendStream};
//use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::Arc;
//use std::sync::RwLock;
use tokio::sync::{RwLock, Mutex};
use tracing::{debug, error, info, warn, trace};
pub(crate) enum QuicTalkSessionState {
    Incoming,
    Handshaking,
    Established,
    Closed,
}

pub(crate) struct QuicTalkSession {
    pub(crate) state: Mutex::<QuicTalkSessionState>,
    pub(crate) conn: Connection,
    pub(crate) recv: RwLock<Option<Box<RecvStream>>>,
    pub(crate) send: RwLock<Option<Box<SendStream>>>,
    // And some user info & connection info
}
impl QuicTalkSession {
    // Closes stream
    pub(crate) async fn close(&self) -> Result<()> {
        // Gracefully close the stream
        let mut sender = self.send.write().await;
        if let Some(ref mut send) = *sender {
            send.finish()
                .await
                .map_err(|e| anyhow!("failed to shutdown stream: {}", e))?;
            // Then delete from session
            *sender = None;
            debug!("Closed stream completely");
            Ok(())
        } else {
            bail!("stream not initialized")
        }
    }
    // Accepts bi-directional stream by server.
    pub(crate) async fn accept(&self) -> Result<()> {
        let stream = self.conn.accept_bi().await;
        let (send, recv) = stream?; // Would generate error when peer closed. Error name: quinn::ConnectionError::ApplicationClosed

        let mut sender = self.send.write().await;
        let mut recver = self.recv.write().await;
        *sender = Some(Box::new(send));
        *recver = Some(Box::new(recv));
        debug!(
            "Accepted a new stream by {remote}",
            remote = self.conn.remote_address()
        );
        Ok(())
    }
    // Opens bi-directional stream by client.
    pub(crate) async fn open(&self) -> Result<()> {
        let stream = self.conn.open_bi().await;
        let (send, recv) = stream?;
        let mut sender = self.send.write().await;
        let mut recver = self.recv.write().await;
        *sender = Some(Box::new(send));
        *recver = Some(Box::new(recv));
        debug!(
            "Opened a new stream to {remote}",
            remote = self.conn.remote_address()
        );
        Ok(())
    }

    // Terminates connection (Only close sessions that are marked closed)
    pub(crate) async fn terminate(&self) -> Result<()> {
        // Should wait_idle and delete session from list after calling this.
        match *self.state.lock().await {
            QuicTalkSessionState::Closed => {
                // Terminate normally
                self.conn.close(0u32.into(), b"done");
                info!(
                    "Connection with {remote} has been successfully terminated.",
                    remote = self.conn.remote_address()
                )
            }
            _ => {
                // State is not closed
                let should_close =
                    self.recv.read().await.is_none() && self.send.read().await.is_none();
                if should_close {
                    *self.state.lock().await = QuicTalkSessionState::Closed;
                    // Kills normally
                    self.conn.close(0u32.into(), b"done");
                    info!(
                        "Connection with {remote} has been successfully terminated.",
                        remote = self.conn.remote_address()
                    )
                    // Give the server a fair chance to receive the close packet
                } else {
                    // Should not terminate
                    bail!("should not terminate a opening connection");
                }
            }
        }
        Ok(())
    }
    // Kills a connection without checking!
    pub(crate) fn kill(&self) {
        self.conn.close(0u32.into(), b"done");
        warn!(
            "Killed connection to {remote} without checking!",
            remote = self.conn.remote_address()
        );
    }

    pub(crate) async fn write(&self, buf: &[u8]) -> Result<usize> {
        match *self.state.lock().await {
            QuicTalkSessionState::Established => {
                let mut sender = self.send.write().await;
                if let Some(ref mut send) = *sender {
                    send.write_all(buf)
                        .await
                        .map_err(|e| anyhow!("failed to send response: {}", e))?;
                } else {
                    bail!("write when stream not initialized")
                }
            }
            _ => {
                bail!("should not write when the connection is not established")
            }
        }
        Ok(buf.len())
    }

    pub(crate) async fn read(&self) -> Result<(usize, Vec<u8>)> {
        match *self.state.lock().await {
            QuicTalkSessionState::Established => {
                let mut recver = self.recv.write().await;
                if let Some(ref mut recv) = *recver {
                    let req = recv
                        .read_to_end(64 * 1024)
                        .await
                        .map_err(|e| anyhow!("failed reading request: {}", e))?;
                    trace!("Received {n} bytes", n = req.len());
                    Ok((req.len(), req))
                } else {
                    bail!("read when stream not initialized")
                }
            }
            _ => {
                bail!("should not read when the connection is not established")
            }
        }
    }
}
