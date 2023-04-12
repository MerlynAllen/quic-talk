use anyhow::{anyhow, bail, Result};
use quinn::{Connection, RecvStream, SendStream};
use std::io::{Read, Write};
use tracing::{debug, error, info, warn};
pub(crate) enum QuicTalkSessionState {
    Incoming,
    Handshaking,
    Established,
    Closed,
}

pub(crate) struct QuicTalkSession {
    pub(crate) state: QuicTalkSessionState,
    pub(crate) conn: Connection,
    pub(crate) recv: Option<Box<RecvStream>>,
    pub(crate) send: Option<Box<SendStream>>,
    // And some user info & connection info
}
impl QuicTalkSession {
    // Closes stream
    pub(crate) async fn close(mut self) -> Result<()> {
        // Gracefully close the stream
        if let Some(mut send) = self.send {
            send.finish()
                .await
                .map_err(|e| anyhow!("failed to shutdown stream: {}", e))?;
            // Then delete from session
            self.send = None;
            debug!("Closed stream completely");
            Ok(())
        } else {
            bail!("stream not initialized")
        }
    }
    // Opens bi-directional stream.
    pub(crate) async fn open(mut self) -> Result<()> {
        let stream = self.conn.accept_bi().await;
        let (send, recv) = stream?; // Would generate error when peer closed. Error name: quinn::ConnectionError::ApplicationClosed
        self.send = Some(Box::new(send));
        self.recv = Some(Box::new(recv));
        debug!(
            "Opened a new stream by {remote}",
            remote = self.conn.remote_address()
        );
        Ok(())
    }
    // Terminates connection (Only close sessions that are marked closed)
    pub(crate) fn terminate(mut self) -> Result<()> {
        // Should wait_idle and delete session from list after calling this.
        match self.state {
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
                let should_close = self.recv.is_none() && self.send.is_none();
                if should_close {
                    self.state = QuicTalkSessionState::Closed;
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
    pub(crate) fn kill(mut self) {
        self.conn.close(0u32.into(), b"done");
        warn!(
            "Killed connection to {remote} without checking!",
            remote = self.conn.remote_address()
        );
    }

    async fn write(self, buf: &[u8]) -> Result<usize> {
        match self.state {
            QuicTalkSessionState::Established => {
                if let Some(mut send) = self.send {
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

    async fn read(self, buf: &mut [u8]) -> Result<(usize, Vec<u8>)> {
        match self.state {
            QuicTalkSessionState::Established => {
                if let Some(mut recv) = self.recv {
                    let req = recv
                        .read_to_end(64 * 1024)
                        .await
                        .map_err(|e| anyhow!("failed reading request: {}", e))?;
                    Ok((buf.len(), req))
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
