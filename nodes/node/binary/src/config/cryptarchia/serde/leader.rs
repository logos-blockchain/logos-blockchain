use lb_core::mantle::{Value, gas::GasCost};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub wallet: WalletConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WalletConfig {
    // Hard cap on the ransaction fee for LEADER_CLAIM
    pub max_tx_fee: GasCost,
}

impl Default for WalletConfig {
    fn default() -> Self {
        Self {
            max_tx_fee: GasCost::new(Value::MAX),
        }
    }
}
