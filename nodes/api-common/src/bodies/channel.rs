use lb_core::{
    header::HeaderId,
    mantle::{
        TxHash,
        gas::GasCost,
        ops::{channel::deposit::DepositOp, transfer::TransferOp},
    },
};
use lb_key_management_system_keys::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositRequestBody {
    pub tip: Option<HeaderId>,
    pub deposit: DepositOp,
    pub burn: TransferOp,
    pub change_public_key: ZkPublicKey,
    pub funding_public_keys: Vec<ZkPublicKey>,
    pub max_tx_fee: GasCost,
}

#[derive(Serialize, Deserialize)]
pub struct ChannelDepositResponseBody {
    pub hash: TxHash,
}
