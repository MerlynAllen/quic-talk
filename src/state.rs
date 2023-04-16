use crate::networking::session::QuicTalkSession;
/// State Machine for QuicTalk
use std::sync::{Arc, RwLock};
#[derive(Clone)]
pub(crate) struct QuicTalkState {
    pub(crate) sessions: Arc<RwLock<Vec<Arc<QuicTalkSession>>>>,
}

impl QuicTalkState {
    pub(crate) fn new() -> Self {
        let sessions_list = Arc::new(RwLock::new(Vec::with_capacity(10)));
        QuicTalkState {
            sessions: sessions_list,
        }
    }
}
