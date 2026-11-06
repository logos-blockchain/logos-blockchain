use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // The key the leader rewards are paid to, which the wallet tracks.
    pub funding_pk: ZkPublicKey,
}
