use serde::{Deserialize, Serialize};

use crate::mantle::{Value, ops::channel::ChannelId};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub amount: Value,
    pub metadata: Vec<u8>,
}
