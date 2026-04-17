use serde::{Deserialize, Serialize};

use crate::mantle::{
    encoding::encode_channel_withdraw,
    ledger::Outputs,
    ops::{OpId, channel::ChannelId},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub outputs: Outputs,
    pub withdraw_nonce: u32,
}

impl OpId for ChannelWithdrawOp {
    fn op_bytes(&self) -> Vec<u8> {
        encode_channel_withdraw(self)
    }
}
