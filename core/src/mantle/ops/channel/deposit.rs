use serde::{Deserialize, Serialize};

use crate::mantle::{ledger::Inputs, ops::channel::ChannelId};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DepositOp {
    pub channel_id: ChannelId,
    pub inputs: Inputs,
    pub metadata: Vec<u8>,
}
