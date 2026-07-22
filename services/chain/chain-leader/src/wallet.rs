use lb_core::mantle::gas::GasCost;
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Hard cap on the transaction fee for LEADER_CLAIM.
    pub max_tx_fee: GasCost,

    // The key to use for paying transaction fees for LEADER_CLAIM.
    // Change notes will be returned to this same funding pk.
    pub funding_pk: ZkPublicKey,
}
