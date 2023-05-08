/// TODO currently a dummy UI implementation using a simple text-based UI
/// this should be replaced with a proper UI implementation
///
///
use std::{
    thread,
};
use tokio::io::{self, *};
use tokio::{task::JoinHandle};
use anyhow::{anyhow, bail, Context, Result};
use tracing::{debug, error, info, info_span, Level, trace};
use crate::{SHOULD_CLOSE, MESSAGES, Message, SESSIONS, ME, message::Body};


pub(crate) struct UI {}

impl UI {
    pub(crate) fn console() -> Result<JoinHandle<Result<()>>> {
        let task = tokio::spawn(async move {
            debug!("Starting console");
            let _: JoinHandle<Result<()>> = tokio::spawn(async move {
                let stdin = io::stdin();
                let buf_reader = io::BufReader::new(stdin);
                let mut lines = buf_reader.lines();

                while let Some(line) = lines.next_line().await? {
                    debug!("Got input {input:?} from stdin.", input = line);
                    let input = line.strip_suffix("\n").unwrap_or(&line).to_string();
                    let sessions = SESSIONS.read().await;
                    let session_index = 0;
                    debug!("Currently have {len} sessions.", len = sessions.len());
                    let session = sessions.get(0).unwrap();
                    let me = match ME.read().await.clone() {
                        Some(me) => me,
                        None => bail!("empty user"),
                    };
                    let msg = Message {
                        time: std::time::SystemTime::now(),
                        user: me,
                        data: Body::String(input.clone()),
                    };
                    session.send_message(msg).await?;
                    debug!("Pushed message {msg:?} to session {session_index}.", msg = input);
                }
                Ok(())
            });
            let _: JoinHandle<Result<()>> = tokio::spawn(async move {
                loop {
                    // TODO check config for client
                    let mut messages_queue = MESSAGES.write().await;
                    let messages = messages_queue.drain(..);
                    if messages.len() == 0 {
                        continue;
                    }
                    debug!("MESSAGE queue have {len} messages.", len = messages.len());

                    io::stdout().flush().await?; // will block when stdin is reading
                    for message in messages {
                        debug!("Got message {msg:?} from MESSAGES queue.",
                    msg = message);
                        match message.data {
                            Body::String(s) => {
                                println!("[Got message from {username}]{s}", username = message.user.username);
                            }
                            Body::File(f) => {
                                println!("[Got new file from {username}] {f:?}", username = message.user.username);
                            }
                            Body::Bytes(i) => {
                                println!("[Got bytes from {username}]{i:?}", username = message.user.username);
                            }
                            _ => {
                                println!("[Got unknown message]: {message:?}");
                            }
                        }
                    }
                }
            });
            Ok(())
        });
        Ok(task)
    }
    pub(crate) fn read(&self) -> Option<String> {
        todo!();
    }
    pub(crate) fn write(&self, text: &str) {
        todo!();
    }
}