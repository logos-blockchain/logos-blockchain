use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        gas::GasCost,
        ops::channel::{ChannelId, deposit::Metadata},
        transactions::hash::TxHash,
    },
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositRequestBody {
    pub tip: Option<HeaderId>,
    pub channel_id: ChannelId,
    pub amount: Value,
    /// The key the deposited note is paid to in the channel.
    pub holder_public_key: ZkPublicKey,
    pub metadata: Metadata,
    pub change_public_key: ZkPublicKey,
    pub funding_public_keys: Vec<ZkPublicKey>,
    pub max_tx_fee: GasCost,
}

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositResponseBody {
    pub hash: TxHash,
    /// The note paid to the holder in the channel. A holder whose wallet is not
    /// this node's imports it to withdraw it.
    pub holder_note: Note,
}
