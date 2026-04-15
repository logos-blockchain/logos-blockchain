use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A structured application message with a unique ID for deduplication.
///
/// Real sequencers need to distinguish "same content published twice" from
/// "same logical message re-published after a reorg". The `tx_id` field
/// provides this: each user action gets a unique ID, and conflict resolution
/// checks whether that ID is already on the canonical branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMessage {
    pub tx_id: Uuid,
    pub text: String,
}

impl AppMessage {
    pub fn new(text: String) -> Self {
        Self {
            tx_id: Uuid::new_v4(),
            text,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("AppMessage serialization should not fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}
