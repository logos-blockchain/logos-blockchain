use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_sdp_service::Declaration;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub declaration: Option<Declaration>,
    pub wallet: WalletConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    pub max_tx_fee: Value,
    pub funding_pk: ZkPublicKey,
}
