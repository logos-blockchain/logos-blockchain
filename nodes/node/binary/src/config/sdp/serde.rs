use std::num::NonZeroU64;

use lb_core::{
    mantle::{Value, gas::GasCost},
    sdp::DeclarationId,
};
use lb_key_management_system_service::backend::preload::KeyId;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Declaration ID (if set, full declaration info will be fetched from
    /// ledger on startup).
    #[serde(default)]
    pub declaration_id: Option<DeclarationId>,
    pub wallet: WalletConfig,
    #[serde(default)]
    pub active_message_tracker: ActiveMessageTrackerConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: GasCost,
    pub funding_key_id: KeyId,
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

pub struct RequiredValues {
    pub funding_key_id: KeyId,
}

impl Config {
    #[must_use]
    pub fn with_required_values(RequiredValues { funding_key_id }: RequiredValues) -> Self {
        Self {
            wallet: WalletConfig {
                funding_key_id,
                max_tx_fee: default_max_tx_fee(),
            },
            declaration_id: None,
            active_message_tracker: ActiveMessageTrackerConfig::default(),
        }
    }

    pub fn set_funding_key_id(&mut self, key_id: KeyId) {
        self.wallet.funding_key_id = key_id;
    }
}
