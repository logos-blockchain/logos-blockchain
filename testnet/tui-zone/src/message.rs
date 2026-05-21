use lb_core::mantle::ops::channel::MsgId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Application-level message wrapper. `tx_uuid` ensures unique payload to
/// avoid mempool deduplication even with same signing keys in decentralized
/// scenarios.
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

/// A single message tracked by the TUI.
///
/// `msg_id` is SDK-provided lineage anchor; `text` is the app-level text
/// extracted from the JSON-encoded payload (falls back to raw UTF-8 for
/// payloads that didn't come from this TUI).
#[derive(Debug, Clone)]
pub struct Msg {
    pub msg_id: MsgId,
    pub text: String,
}

impl Msg {
    pub fn from_payload(msg_id: MsgId, payload: &[u8]) -> Self {
        let text = AppMessage::from_bytes(payload)
            .map_or_else(|| String::from_utf8_lossy(payload).into_owned(), |m| m.text);
        Self { msg_id, text }
    }
}
