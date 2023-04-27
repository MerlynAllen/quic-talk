use anyhow::{anyhow, bail, Result};
use quinn::{Connection, RecvStream, SendStream};
//use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
//use std::sync::RwLock;
use crate::{
    Message,
    networking::Stream,
    SHOULD_CLOSE,
    MESSAGES,
};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, trace, warn};
use tracing::field::debug;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    Ready,
    // QUIC connection established
    Authenticating,
    // Sending / receiving auth data
    Established,
    // Authentication successfully completed
    Closed,         // QUIC connection closed
}

pub(crate) struct Session {
    pub(crate) state: RwLock<SessionState>,
    pub(crate) conn: Connection,
    handler: Arc<RwLock<Option<tokio::task::JoinHandle<Result<()>>>>>,
    streams: Arc<RwLock<Vec<Stream>>>,
    send: Arc<RwLock<Vec<Message>>>,
    // Messages to be sent
    recv: Arc<RwLock<Vec<Message>>>, // Messages received
    // And some user info & connection info
}


// TODO should build a own error type, especially for Messages.
// messages failed to send should be put back to the send queue.

impl Session {
    // Spawn a new task to handle session events, returns JoinHandle
    pub(crate) async fn new(conn: Connection) -> Arc<Self> {
        let this = Arc::new(Self {
            state: RwLock::new(SessionState::Ready),
            conn,
            handler: Arc::new(RwLock::new(None)),
            streams: Arc::new(RwLock::new(Vec::new())),
            send: Arc::new(RwLock::new(Vec::new())),
            recv: Arc::new(RwLock::new(Vec::new())),
        });
        let ret = this.clone();
        let handler = this.handler.clone();
        *handler.write().await = Some(tokio::spawn(async move {
            // Clone `this` into multiple copies to move into different scopes.
            let main_this = this.clone();
            let listener_this = this.clone();
            let dispatcher_this = this.clone();
            let receiver_this = this.clone();
            let sender_this = this.clone();


            // First authenticate
            let this = main_this;
            this.set_state(SessionState::Authenticating).await;
            this.auth().await?; // if error, throw and return.
            this.verify().await?; // if error, throw and return.
            // Established
            this.set_state(SessionState::Established).await;
            let receive_stream_handlers: Arc<Mutex<Vec<JoinHandle<Result<Message>>>>> = Arc::new(Mutex::new(Vec::new()));
            let receive_stream_handlers_for_listener = receive_stream_handlers.clone();

            let send_stream_handlers: Arc<Mutex<Vec<JoinHandle<Result<()>>>>> = Arc::new(Mutex::new(Vec::new()));
            let send_stream_handlers_for_sender = send_stream_handlers.clone();


            // Then spawn a listener
            let this = listener_this;
            let listener: JoinHandle<Result<()>> = tokio::spawn(async move { // `this` moved to subprocess
                // Wait for session state to become Established
                loop {
                    // Check exit condition
                    debug!("Checking session state");
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }
                    // Wait for session state to become Established
                    if *this.state.read().await == SessionState::Established {
                        break;
                    }
                    debug!("Waiting for session state to become Established");
                    time::sleep(time::Duration::from_millis(100)).await;
                }
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }

                    // Wait for new stream
                    match this.clone().accept().await {
                        Ok(task) => {
                            receive_stream_handlers_for_listener.clone().lock().await.push(task);
                        }
                        Err(e) => {
                            error!("Failed to accept new stream: {}", e);
                            // QUIC Idle Timed Out
                            break;
                        }
                    }; // accept opens a new stream and spawns a new task to handle it.
                }
                Ok(())
            });


            // Receive messages from opening streams created by listener
            let this = receiver_this;
            let receiver: JoinHandle<Result<()>> = tokio::spawn(async move {
                // Get message from listener. Check if the message sending is done.
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }
                    // Enumerate the list and push to the message pool
                    for task in receive_stream_handlers.lock().await.drain(..) {
                        // Check if the task is done
                        match task.await {
                            Ok(msg) => {
                                match msg {
                                    Ok(msg) => {
                                        // Push the message to the message pool
                                        MESSAGES.write().await.push(msg);
                                    }
                                    Err(e) => {
                                        error!("Failed to get message from listener: {}", e);
                                        continue;
                                    }
                                };
                            }
                            Err(e) => {
                                error!("Failed to get message from listener: {}", e);
                                continue;
                            }
                        };
                    }
                }
                Ok(())
            });

            let this = dispatcher_this;
            let dispatcher: JoinHandle<Result<()>> = tokio::spawn(async move {
                // Loop and send messages from `self.send` to peer
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }
                    // TODO should open a new task
                    let this = this.clone();
                    let mut send_list = this.send.write().await;
                    // If there are messages to be sent
                    let msg = match send_list.pop() {
                        Some(msg) => {
                            debug!("Got message from send queue");
                            debug!("Queue length: {}", send_list.len());
                            msg
                        },
                        None => {
                            // No messages to be sent, wait for new messages.
                            continue;
                        }
                    };
                    // Send the message
                    let this = this.clone();
                    match this.send(msg).await {
                        Ok(task) => {
                            // Message stream scheduled to be sent
                            send_stream_handlers_for_sender.lock().await.push(task);
                            continue;
                        }
                        Err(e) => {
                            error!("Failed to send message: {}", e);
                            continue;
                        }
                    };
                }
                Ok(())
            });


            // Then a sender to await all messages to be sent
            // This task is kind of like a garbage collector
            let this = sender_this;
            let sender: JoinHandle<Result<()>> = tokio::spawn(async move {
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }
                    // Enumerate the list and wait
                    for task in send_stream_handlers.lock().await.drain(..) {
                        // Check if the task is done
                        match task.await {
                            Ok(_) => {
                                // Task done
                                continue;
                            }
                            Err(e) => {
                                error!("Failed to send message: {}", e);
                                continue;
                            }
                        };
                    }
                }

                Ok(())
            });
            // Join sub-tasks.
            listener.await??;
            dispatcher.await??;
            sender.await??;
            receiver.await??;
            Ok(())
        }));
        // Return a new instance
        ret
    }
    /// Authenticate with peer
    async fn auth(&self) -> Result<()> {
        // Temporarily not implemented
        Ok(())
    }
    /// Verify peer
    async fn verify(&self) -> Result<()> {
        // Temporarily not implemented
        Ok(())
    }

    pub(crate) async fn set_state(&self, state: SessionState) {
        *self.state.write().await = state;
    }

    pub(crate) async fn get_state(&self) -> SessionState {
        *self.state.read().await
    }


    /// Accepts a new stream and spawns a new task to read all contents.
    /// The task should expect a new message or error.
    /// Then reply with a message.
    /// Then closes the stream.
    async fn accept(self: Arc<Self>) -> Result<tokio::task::JoinHandle<Result<Message>>> {
        debug!("Waiting for new stream...");
        let stream = self.conn.accept_bi().await?; // This would block?
        let (mut send, mut recv) = stream;
        // Would generate error when peer closed. Error name: quinn::ConnectionError::ApplicationClosed
        debug!(
            "Accepted a new stream opened by {remote}",
            remote = self.conn.remote_address()
        );
        let task = tokio::spawn(async move {
            // First, read data from stream
            let mut buf = [0u8; 1024];
            let mut msg = Vec::new();
            debug!("Reading data from stream...");
            loop {
                // Check exit condition
                if *self.state.read().await == SessionState::Closed {
                    break;
                }

                let n = recv.read(&mut buf).await?;
                debug!("Read {:?} bytes from stream", n);
                match n {
                    Some(0) | None => break,
                    Some(n) =>
                        msg.extend_from_slice(&buf[..n]),
                }
            }
            debug!("Read data from stream: {:?}", msg);
            // Then, parse data
            let msg = Message::from_bytes(Vec::from(buf));
            // recv do not need to close, it will be closed by the sender.
            // Then, reply with `send`
            // TODO: reply with a structured message but not simply a string.
            let reply = "OK";
            send.write_all(reply.as_bytes()).await?;
            // Then, close stream
            send.finish().await?;
            Ok(msg)
        });
        Ok(task)
    }


    /// Sends a message to peer.
    /// Starts a new task to send message. Returns a join handle to be awaited by caller.
    async fn send(self: Arc<Self>, msg: Message, ) -> Result<JoinHandle<Result<()>>> {
        // If there are messages to send, create a new stream and send it.
        let (mut send, mut recv) = self.conn.open_bi().await?;
        debug!("Opened a new stream to send message");
        let task: JoinHandle<Result<()>> = tokio::spawn(async move {
            let write_buf = match msg.as_bytes() {
                Some(buf) => buf,
                None => {
                    bail!("string convert to bytes should not fail");
                }
            };
            debug!("Sending message: {:?}", msg.as_string());
            send.write_all(&write_buf).await?;
            send.finish().await?;
            debug!("{} bytes sent", write_buf.len());
            debug!("Waiting for peer to reply echo");
            // Wait for peer to reply echo
            let mut read_buf = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = recv.read(&mut buf).await?;
                match n {
                    Some(0) | None => break,
                    Some(n) => read_buf.extend_from_slice(&buf[..n]),
                }
            }
            debug!("Received reply: {:?}", read_buf);
            // TODO Deserialize message
            // Currently treat all messages as `String`
            let reply = String::from_utf8(read_buf)?;
            match reply.as_str() {
                "OK" => {
                    debug!("Stream transferred data safely.");
                }
                _ => {
                    error!("Failed to receive echo. Putting back to queue.");
                    // Put back to queue
                    let mut send_list = self.send.write().await;
                    send_list.push(msg);
                }
            };
            Ok(())
        });
        Ok(task)
    }


    /// Interface for other modules to send message to peer.
    pub(crate) async fn send_message(&self, msg: Message) -> Result<()> {
        // Push to send queue
        let mut send_list = self.send.write().await;
        send_list.push(msg);
        Ok(())
    }
}
