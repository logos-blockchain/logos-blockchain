#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use lb_core::mantle::Value;
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletConfig {
    // Hard cap on the ransaction fee for LEADER_CLAIM
    #[serde(default = "default_max_tx_fee")]
    #[serde(skip_serializing_if = "is_default_max_tx_fee")]
    pub max_tx_fee: Value,

    // The key to use for paying transaction fees for LEADER_CLAIM.
    // Change notes will be returned to this same funding pk.
    pub funding_pk: ZkPublicKey,
}

#[must_use]
pub const fn default_max_tx_fee() -> Value {
    Value::MAX
}

const fn is_default_max_tx_fee(value: &Value) -> bool {
    *value == default_max_tx_fee()
}
