use std::num::NonZeroU64;

use lb_core::{
    mantle::{Value, gas::GasCost},
    sdp::DeclarationId,
};
use lb_key_management_system_service::keys::ZkPublicKey;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Declaration ID (if set, full declaration info will be fetched from
    /// ledger on startup).
    #[serde(default)]
    pub declaration_id: Option<DeclarationId>,
    pub wallet: WalletConfig,
    #[serde(default = "default_active_message_tracker")]
    pub active_message_tracker: ActiveMessageTrackerConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WalletConfig {
    #[serde(default = "default_max_tx_fee")]
    pub max_tx_fee: GasCost,
    pub funding_pk: ZkPublicKey,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActiveMessageTrackerConfig {
    /// Interval between status checks of a submitted activity, in tip changes.
    #[serde(default = "default_status_check_interval_in_tip_changes")]
    pub status_check_interval_in_tip_changes: NonZeroU64,
    /// Max number of status checks for a submitted activity.
    #[serde(default = "default_max_status_checks")]
    pub max_status_checks: NonZeroU64,
}

const fn default_max_tx_fee() -> GasCost {
    GasCost::new(Value::MAX)
}

const fn default_status_check_interval_in_tip_changes() -> NonZeroU64 {
    NonZeroU64::new(3).unwrap()
}

const fn default_max_status_checks() -> NonZeroU64 {
    NonZeroU64::new(5).unwrap()
}

const fn default_active_message_tracker() -> ActiveMessageTrackerConfig {
    ActiveMessageTrackerConfig {
        status_check_interval_in_tip_changes: default_status_check_interval_in_tip_changes(),
        max_status_checks: default_max_status_checks(),
    }
}

pub struct RequiredValues {
    pub funding_pk: ZkPublicKey,
}

impl Config {
    #[must_use]
    pub const fn with_required_values(RequiredValues { funding_pk }: RequiredValues) -> Self {
        Self {
            wallet: WalletConfig {
                funding_pk,
                max_tx_fee: default_max_tx_fee(),
            },
            declaration_id: None,
            active_message_tracker: default_active_message_tracker(),
        }
    }

    pub const fn set_funding_pk(&mut self, pk: ZkPublicKey) {
        self.wallet.funding_pk = pk;
    }
}
