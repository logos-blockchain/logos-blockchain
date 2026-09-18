use lb_core::mantle::gas::GasCost;
use lb_key_management_system_service::backend::preload::KeyId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Hard cap on the transaction fee for LEADER_CLAIM.
    pub max_tx_fee: GasCost,

    // The KMS id of the key to use for paying transaction fees for
    // LEADER_CLAIM. Change notes will be returned to this same key.
    pub funding_key_id: KeyId,
}
