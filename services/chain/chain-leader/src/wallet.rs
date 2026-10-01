use lb_core::mantle::gas::GasCost;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderWalletConfig {
    // Hard cap on the transaction fee for LEADER_CLAIM.
    pub max_tx_fee: GasCost,
}
