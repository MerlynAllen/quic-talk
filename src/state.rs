use crate::networking::session::QuicTalkSession;
/// State Machine for QuicTalk
use std::sync::{Arc, Mutex};
pub(crate) struct QuicTalkState {
    pub(crate) sessions: Arc<Mutex<Vec<QuicTalkSession>>>,
}

impl QuicTalkState {
    pub(crate) fn new() -> Self {
        let sessions_list = Arc::new(Mutex::new(Vec::with_capacity(10)));
        QuicTalkState {
            sessions: sessions_list,
        }
    }
}
