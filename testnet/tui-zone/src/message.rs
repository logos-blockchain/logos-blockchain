use lb_core::mantle::ops::channel::MsgId;

/// A single message tracked by the TUI.
///
/// `msg_id` is SDK-provided lineage anchor; `text` is the raw payload bytes
/// interpreted as UTF-8.
#[derive(Debug, Clone)]
pub struct Msg {
    pub msg_id: MsgId,
    pub text: String,
}

impl Msg {
    pub fn from_payload(msg_id: MsgId, payload: &[u8]) -> Self {
        Self {
            msg_id,
            text: String::from_utf8_lossy(payload).into_owned(),
        }
    }
}
