use nomos_core::mantle::ops::channel::{ChannelId, Ed25519PublicKey, MsgId};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct DispersalRequestBody {
    pub channel_id: ChannelId,
    pub parent_msg_id: MsgId,
    pub signer: Ed25519PublicKey,
    pub data: Vec<u8>,
}
