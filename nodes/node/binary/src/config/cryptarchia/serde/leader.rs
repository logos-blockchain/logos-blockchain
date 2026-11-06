use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    // The key the leader rewards are paid to. It must be in `wallet.known_keys`,
    // or the node refuses to start.
    pub funding_pk: ZkPublicKey,
}
