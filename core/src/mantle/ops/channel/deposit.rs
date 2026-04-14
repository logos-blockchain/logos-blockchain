use serde::{Deserialize, Serialize};

use crate::mantle::{NoteId, ops::channel::ChannelId};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub inputs: Vec<NoteId>,
    pub metadata: Vec<u8>,
}
