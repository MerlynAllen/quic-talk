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
use crate::{SHOULD_CLOSE, MESSAGES, Message, STDIN_BUFFER, SESSIONS};


pub(crate) struct UI {}

impl UI {
    pub(crate) fn console() -> Result<JoinHandle<Result<()>>> {
        let task = tokio::spawn(async move {
            debug!("Starting console");
            let stdin_handler = tokio::spawn(async move {});

            let stdin = io::stdin();
            let buf_reader = io::BufReader::new(stdin);
            let mut lines = buf_reader.lines();

            print!("<<");
            while let Some(line) = lines.next_line().await? {

                // TODO check config for client
                if *SHOULD_CLOSE.clone().read().await {
                    break;
                }

                print!("<<");

                debug!("Got input {input:?} from stdin.", input = line);
                let input = line.strip_suffix("\n").unwrap_or(&line).to_string();
                let sessions = SESSIONS.read().await;
                let session_index = 0;
                debug!("Currently have {len} sessions.", len = sessions.len());
                let session = sessions.get(0).unwrap();
                session.send_message(Message::from_string(input.clone())).await?;
                debug!("Pushed message {msg:?} to session {session_index}.", msg = input);
            }
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