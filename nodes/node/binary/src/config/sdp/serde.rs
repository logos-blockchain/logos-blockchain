use lb_core::{
    mantle::{NoteId, Value},
    sdp::DeclarationId,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub declaration: Option<Declaration>,
    pub wallet: WalletConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Declaration {
    pub id: DeclarationId,
    pub zk_id: ZkPublicKey,
    pub locked_note_id: NoteId,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    pub max_tx_fee: Value,
    pub funding_pk: ZkPublicKey,
}
