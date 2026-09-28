use std::num::NonZeroU64;

use lb_core::{
    mantle::{Value, gas::GasCost},
    sdp::DeclarationId,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    /// Declaration ID (if set, full declaration info will be fetched from
    /// ledger on startup).
    #[serde(default)]
    pub declaration_id: Option<DeclarationId>,
    #[serde(default)]
    pub wallet: WalletConfig,
    #[serde(default)]
    pub active_message_tracker: ActiveMessageTrackerConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct ActiveMessageTrackerConfig {
    /// Interval between status checks of a submitted activity, in tip changes.
    pub status_check_interval_in_tip_changes: NonZeroU64,
}

impl Default for ActiveMessageTrackerConfig {
    fn default() -> Self {
        Self {
            status_check_interval_in_tip_changes: NonZeroU64::new(3).unwrap(),
        }
    }
}

const fn default_max_tx_fee() -> GasCost {
    GasCost::new(Value::MAX)
}
