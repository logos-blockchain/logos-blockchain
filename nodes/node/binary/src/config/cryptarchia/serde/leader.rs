use lb_core::mantle::{Value, gas::GasCost};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletConfig {
    // Hard cap on the ransaction fee for LEADER_CLAIM
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: GasCost,
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self {
            max_tx_fee: default_max_tx_fee(),
        }
    }
}

#[must_use]
pub const fn default_max_tx_fee() -> GasCost {
    GasCost::new(Value::MAX)
}
