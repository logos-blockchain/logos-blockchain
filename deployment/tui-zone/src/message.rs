use lb_core::mantle::ops::channel::MsgId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Application-level message wrapper. `tx_uuid` ensures unique payload to
/// avoid mempool deduplication even with same signing keys in decentralized
/// scenarios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMessage {
    /// Random transaction UUID used to make otherwise identical payloads
    /// unique.
    pub tx_uuid: Uuid,
    /// User-entered message text.
    pub text: String,
}

impl AppMessage {
    /// Create a new application message with a fresh UUID.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self {
            tx_uuid: Uuid::new_v4(),
            text,
        }
    }

    /// Serialize the message to bytes for inscription payloads.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("AppMessage serialization should not fail")
    }

    /// Deserialize an application message from inscription payload bytes.
    #[must_use]
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
    /// SDK message lineage identifier.
    pub msg_id: MsgId,
    /// Text rendered in the TUI.
    pub text: String,
}

impl Msg {
    /// Build a rendered message from an inscription payload.
    #[must_use]
    pub fn from_payload(msg_id: MsgId, payload: &[u8]) -> Self {
        let text = AppMessage::from_bytes(payload)
            .map_or_else(|| String::from_utf8_lossy(payload).into_owned(), |m| m.text);
        Self { msg_id, text }
    }
}
