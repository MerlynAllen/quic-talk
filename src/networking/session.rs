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
    ME,
    User,
    message::{Control, Body},
};
use tokio::sync::{Mutex, RwLock};
use std::cell::RefCell;
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, trace, warn};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use crate::networking::{MAX_AUTH_READ_LEN, MAX_STREAM_LEN};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionState {
    Ready,
    // QUIC connection established
    Authenticating,
    // Sending / receiving auth data
    Established,
    // Authentication successfully completed
    Closed,
    // QUIC connection closed
    Terminated,     // Session terminated
}

pub(crate) struct Session {
    pub(crate) state: RwLock<SessionState>,
    pub(crate) conn: Connection,
    handler: Arc<RwLock<Option<JoinHandle<Result<()>>>>>,
    streams: Arc<RwLock<Vec<Stream>>>,
    send: Arc<RwLock<Vec<Message>>>,
    // Messages to be sent
    recv: Arc<RwLock<Vec<Message>>>,
    // Messages received
    // And some user info & connection info
    peer: RwLock<Option<User>>,
    me: Option<User>,
}


// TODO should build a own error type, especially for Messages.
// messages failed to send should be put back to the send queue.

impl Session {
    // Spawn a new task to handle session events, returns JoinHandle
    pub(crate) async fn new(conn: Connection) -> Result<Arc<Self>> {
        let me = ME.read().await.clone();
        let this = Arc::new(Self {
            state: RwLock::new(SessionState::Ready),
            conn,
            handler: Arc::new(RwLock::new(None)),
            streams: Arc::new(RwLock::new(Vec::new())),
            send: Arc::new(RwLock::new(Vec::new())),
            recv: Arc::new(RwLock::new(Vec::new())),
            peer: RwLock::new(None),
            me,
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
            let collector_this = this.clone();

            // First authenticate
            let this = main_this;
            debug!("Session created. Authenticating...");
            this.set_state(SessionState::Authenticating).await;
            let auth_this = this.clone();
            let auth_task = tokio::spawn(auth_this.auth());
            let verify_this = this.clone();
            let verify_task = tokio::spawn(verify_this.verify());

            // Wait for authentication to complete
            auth_task.await??;
            verify_task.await??;
            // Established
            this.set_state(SessionState::Established).await;
            let receive_stream_handles: Arc<Mutex<Vec<JoinHandle<Result<Message>>>>> = Arc::new(Mutex::new(Vec::new()));
            let receive_stream_handles_for_listener = receive_stream_handles.clone();

            let send_stream_handles: Arc<Mutex<Vec<JoinHandle<Result<()>>>>> = Arc::new(Mutex::new(Vec::new()));
            let send_stream_handles_for_sender = send_stream_handles.clone();

            // Then spawn a listener
            let this = listener_this;
            let _listener: JoinHandle<Result<()>> = tokio::spawn(async move { // `this` moved to subprocess
                // Wait for session state to become Established
                loop {
                    if *this.state.read().await == SessionState::Closed {
                        break;
                    }
                    // Wait for session state to become Established
                    if *this.state.read().await == SessionState::Established {
                        break;
                    }
                    debug!("Waiting for session state to become Established");
                }
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        debug!("Session closed. Exiting listener.");
                        break;
                    }

                    // Wait for new stream
                    match this.clone().accept().await {
                        Ok(task) => {
                            receive_stream_handles_for_listener.clone().lock().await.push(task);
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
                        debug!("Session closed. Exiting receiver.");
                        break;
                    }
                    // Enumerate the list and push to the message pool
                    for task in receive_stream_handles.lock().await.drain(..) {
                        // Check if the task is done
                        match task.await {
                            Ok(res) => {
                                match res {
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
            let _dispatcher: JoinHandle<Result<()>> = tokio::spawn(async move {
                // Loop and send messages from `self.send` to peer
                loop {
                    // Check exit condition
                    if *this.state.read().await == SessionState::Closed {
                        debug!("Session closed. Exiting dispatcher.");
                        break;
                    }
                    let this = this.clone();
                    let mut send_list = this.send.write().await;
                    // If there are messages to be sent
                    let msg = match send_list.pop() {
                        Some(msg) => {
                            debug!("Got message from send queue");
                            debug!("Queue length: {}", send_list.len());
                            msg
                        }
                        None => {
                            // No messages to be sent, wait for new messages.
                            continue;
                        }
                    };
                    // Send the message
                    let this = this.clone();
                    match this.send(msg).await { // Return a new task
                        Ok(task) => {
                            // Message stream scheduled to be sent
                            send_stream_handles_for_sender.lock().await.push(task);
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
                        debug!("Session closed. Exiting sender.");
                        break;
                    }
                    // Enumerate the list and wait
                    for task in send_stream_handles.lock().await.drain(..) {
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
            sender.await??;
            receiver.await??;
            let this = collector_this;
            this.set_state(SessionState::Terminated).await;
            // Set to terminated and wait for garbage collection
            debug!("Session terminated. Waiting for garbage collection.");
            Ok(())
        }));
        // Return a new instance
        Ok(ret)
    }
    /// Authenticate with peer
    async fn auth(self: Arc<Self>) -> Result<()> {
        // Should be waited.
        // Send a message to peer
        debug!("Authenticating with peer");
        match self.me {
            Some(ref me) => {
                // TODO send verification message
                let msg = Message {
                    user: me.clone(),
                    time: SystemTime::now(),
                    data: Body::Auth,
                };
                let msg = serde_json::to_vec(&msg)?;
                debug!("Sending auth message: {:?}", msg);
                // write immediately
                let (mut send, mut recv) = self.conn.open_bi().await?;
                send.write_all(msg.as_slice()).await?;
                send.finish().await?;
                debug!("Auth message sent, {} bytes. Waiting for reply.", msg.len());
                // wait for reply
                let buf = recv.read_to_end(MAX_AUTH_READ_LEN).await?;
                let msg: Message = serde_json::from_slice(buf.as_slice())?;
                debug!("Got reply: {:?}", msg);
                match msg.data {
                    Body::Control(c) => {
                        match c {
                            Control::Verified => {
                                debug!("Peer verified");
                            }
                            Control::ConnectionDenied => {
                                debug!("Peer denied connection");
                                bail!("connection denied")
                            }
                            _ => {
                                debug!("Peer replied with unexpected control message");
                                bail!("unexpected control message")
                            }
                        }
                    }
                    _ => {
                        debug!("Peer replied with unexpected message type");
                        bail!("unexpected message type")
                    }
                };
            }
            None => {
                debug!("User is empty");
                bail!("user empty")
            }
        }
        Ok(())
    }
    /// Verify peer
    async fn verify(self: Arc<Self>) -> Result<()> {
        // Should be waited.
        match self.me {
            Some(ref me) => {
                // wait for peer auth
                debug!("Waiting for peer auth");
                let (mut send, mut recv) = self.conn.accept_bi().await?;
                debug!("Got new stream");
                let buf = recv.read_to_end(MAX_AUTH_READ_LEN).await?;
                let msg: Message = serde_json::from_slice(buf.as_slice())?;
                debug!("Read auth message from peer stream {} bytes: {:?}", buf.len(), msg);
                match msg.data {
                    Body::Auth => {
                        // TODO Verify the user
                        debug!("Auth message received, verifying");
                        debug!("Peer verified. Reply to peer.");
                        let reply = Message {
                            user: me.clone(),
                            time: SystemTime::now(),
                            data: Body::Control(Control::Verified),
                        };
                        let reply = serde_json::to_vec(&reply)?;
                        // write immediately
                        send.write_all(reply.as_slice()).await?;
                        debug!("Reply sent.");
                        send.finish().await?;
                        debug!("Stream finished.");
                        // write peer info to self.peer
                        *self.peer.write().await = Some(msg.user.clone());
                        debug!("Peer verified. Peer info: {:?}", self.peer);
                    }
                    _ => {
                        debug!("Peer sent unexpected message type");
                        bail!("unexpected message type")
                    }
                };
            }
            None => {
                debug!("User is empty");
                bail!("user empty")
            }
        }
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
    async fn accept(self: Arc<Self>) -> Result<JoinHandle<Result<Message>>> {
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
            let buf = recv.read_to_end(MAX_STREAM_LEN).await?;
            debug!("Read data from stream: {:?}", buf);
            // Then, parse data
            let msg: Message = serde_json::from_slice(&buf)?;
            // recv do not need to close, it will be closed by the sender.

            // Check if the message is a control message
            match msg.clone().data {
                Body::Control(c) => {
                    match c {
                        Control::CloseByPeer => {
                            // Close the session
                            debug!("Received close control message from peer.");
                            self.set_state(SessionState::Closed).await;
                            return Ok(msg)
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            // Then, reply with `send`
            match self.make_ack() {
                Ok(reply) => {
                    debug!("Sending reply: {:?}", reply);
                    let reply = serde_json::to_string(&reply)?;
                    send.write_all(reply.as_bytes()).await?;
                    debug!("Reply sent. {} bytes. Closing stream.", reply.len());
                }
                Err(e) => {
                    error!("Failed to make ack: {}", e);
                }
            };
            // Then, close stream
            send.finish().await?;
            Ok(msg)
        });
        Ok(task)
    }


    /// Sends a message to peer.
    /// Starts a new task to send message. Returns a join handle to be awaited by caller.
    async fn send(self: Arc<Self>, msg: Message) -> Result<JoinHandle<Result<()>>> {
        // If there are messages to send, create a new stream and send it.
        let (mut send, mut recv) = self.conn.open_bi().await?;
        debug!("Opened a new stream to send message");
        let task: JoinHandle<Result<()>> = tokio::spawn(async move {
            let write_buf = serde_json::to_vec(&msg)?;
            debug!("Sending message: {:?}", msg);
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
            let reply: Message = serde_json::from_slice(&read_buf)?;
            match reply.data {
                Body::Control(c) => {
                    match c {
                        Control::Acknowledge => {
                            debug!("Stream transferred data safely.");
                        }
                        Control::RetransmissionRequest => {
                            error!("Failed to receive echo. Got retransmission request. Putting back to queue.");
                            // Put back to queue
                            let mut send_list = self.send.write().await;
                            send_list.push(msg);
                        }
                        _ => {
                            error!("Failed to receive echo. Wrong control type: {:?}.", c);
                        }
                    }
                }
                _ => {
                    error!("Failed to receive echo. Wrong message type: {:?}", reply.data);
                }
            }
            Ok(())
        });
        Ok(task)
    }
    pub(crate) async fn close(&self) -> Result<()> {
        let close_msg = Message {
            time: SystemTime::now(),
            user: self.me.clone().unwrap(),
            data: Body::Control(Control::CloseByPeer),
        };
        self.send_now(close_msg).await;
        self.set_state(SessionState::Terminated).await;
        Ok(())
    }

    /// Interface for other modules to send message to peer.
    pub(crate) async fn send_message(&self, msg: Message) -> Result<()> {
        // Push to send queue
        let mut send_list = self.send.write().await;
        send_list.push(msg);
        Ok(())
    }

    /// Method to send immediate message to peer.
    /// This method is used to send control message.
    /// It will not be put into send queue and retransmitted.
    /// Ignore all errors.
    async fn send_now(&self, msg: Message) {
        debug!("Sending immediate close control message to peer {peer}.", peer = self.conn.remote_address());
        let (mut send, _) = match self.conn.open_bi().await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to open stream to send message: {}", e);
                return;
            }
        };
        let write_buf = match serde_json::to_vec(&msg) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to serialize message: {}", e);
                return;
            }
        };
        let _ = send.write_all(&write_buf).await;
        let _ = send.finish().await;
    }

    /// Check session state, if it is terminated.
    pub(crate) async fn is_terminated(&self) -> bool {
        *self.state.read().await == SessionState::Terminated
    }


    /// Snippet to make an ack message.
    fn make_ack(&self) -> Result<Message> {
        match self.me {
            Some(ref m) => {
                Ok(Message {
                    time: SystemTime::now(),
                    user: m.clone(),
                    data: Body::Control(Control::Acknowledge),
                })
            }
            None => {
                error!("Failed to make ack. No user information.");
                bail!("user empty")
            }
        }
    }
    pub(crate) async fn get_peername(&self) -> Result<String> {
        // get peer username
        let peername = match *self.peer.read().await {
            Some(ref p) => p.clone().username,
            None => {
                error!("Failed to get peername");
                bail!("failed to get peername")
            }
        };
        Ok(peername)
    }
}
