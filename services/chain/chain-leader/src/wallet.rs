use lb_core::mantle::gas::GasCost;
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Cap on a transaction fee of the leader. The leader submits no transaction,
    // so nothing reads it.
    pub max_tx_fee: GasCost,

    // The key the leader rewards are paid to, which the wallet tracks.
    pub funding_pk: ZkPublicKey,
}
