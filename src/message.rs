use serde::{Deserialize, Serialize};
use std::time::{SystemTime, Duration};
use crate::user::User;


#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Message {
    pub(crate) time: SystemTime,
    pub(crate) user: User,
    pub(crate) data: Body,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) enum Body {
    String(String),
    Bytes(Vec<u8>),
    File(File),
    Auth,
    Control(Control),
}


#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct File {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) enum Control {
    CloseByPeer,
    Acknowledge,
    RetransmissionRequest,
    Verified,
    ConnectionDenied,
}


// pub(crate) enum Message {
//     String(String),
//     Bytes(Vec<u8>),
//
// }
impl Message {}