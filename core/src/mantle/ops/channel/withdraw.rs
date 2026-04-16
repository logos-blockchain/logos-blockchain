use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{
        encoding::encode_channel_withdraw,
        ledger::Outputs,
        ops::{OPERATION_ID_V1, OpId, channel::ChannelId},
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub outputs: Outputs,
    pub withdraw_nonce: u32,
}

impl OpId for ChannelWithdrawOp {
    fn op_id(&self) -> Hash {
        let mut encoded_bytes = OPERATION_ID_V1.clone();
        encoded_bytes.extend(encode_channel_withdraw(self));
        Hasher::digest(&encoded_bytes).into()
    }
}
