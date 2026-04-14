use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Digest as _, Hash, Hasher},
    mantle::{Note, Utxo, encoding::encode_channel_withdraw, ops::channel::ChannelId},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelWithdrawOp {
    pub channel_id: ChannelId,
    pub outputs: Vec<Note>,
    pub withdraw_nonce: u32,
}

impl ChannelWithdrawOp {
    #[must_use]
    pub fn id(&self) -> Hash {
        let encoded_bytes = encode_channel_withdraw(self);
        let mut hasher = Hasher::new();
        hasher.update(b"OPERATION_ID_V1");
        hasher.update(encoded_bytes);
        hasher.finalize().into()
    }

    pub fn utxos(&self) -> impl Iterator<Item = Utxo> + '_ {
        let withdraw_id = self.id();
        self.outputs
            .iter()
            .enumerate()
            .map(move |(index, note)| Utxo {
                op_id: withdraw_id,
                output_index: index,
                note: *note,
            })
    }
}
