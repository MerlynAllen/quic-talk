use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct User {
    pub(crate) username: String,
    pub(crate) email: String,
    pub(crate) user_id: String, // Hash
}

impl User {
    pub(crate) fn new(username: String, email: String, user_id: String) -> Self {
        Self {
            username,
            email,
            user_id,
        }
    }
}