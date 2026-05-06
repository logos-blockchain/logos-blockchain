use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A structured application message with a unique ID for deduplication.
///
/// Real sequencers need to distinguish "same content published twice" from
/// "same logical message re-published after a reorg". The `tx_uuid` field
/// provides this: each user action gets a unique ID, and conflict resolution
/// checks whether that ID is already on the canonical branch.
///
/// Authorship ("did we send this?") is tracked separately by the state layer
/// (`my_submissions`), not on the message itself, so it survives reorgs that
/// remove and re-add the message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMessage {
    pub tx_uuid: Uuid,
    pub text: String,
}

impl AppMessage {
    pub fn new(text: String) -> Self {
        Self {
            tx_uuid: Uuid::new_v4(),
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
