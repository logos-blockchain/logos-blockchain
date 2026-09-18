use lb_core::mantle::{Value, gas::GasCost};
use lb_key_management_system_service::backend::preload::KeyId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    // Hard cap on the ransaction fee for LEADER_CLAIM
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: GasCost,

    // The KMS id of the key to use for paying transaction fees for
    // LEADER_CLAIM. Change notes will be returned to this same key.
    pub funding_key_id: KeyId,
}

#[must_use]
pub const fn default_max_tx_fee() -> GasCost {
    GasCost::new(Value::MAX)
}
