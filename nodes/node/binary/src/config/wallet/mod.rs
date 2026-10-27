use lb_cryptarchia_engine::time::EraSchedule;
use lb_services_utils::overwatch::RecoveryData;
use lb_time::era::EpochLength as _;
use lb_wallet_service::{WalletEraSettings, WalletServiceSettings};

use crate::config::{
    deployment::{EraDefinition, era::ruleset::EraRuleset},
    wallet::serde::Config,
};

pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_wallet_service_settings(
        self,
        recovery_data: RecoveryData,
        eras: &EraSchedule<EraDefinition>,
    ) -> WalletServiceSettings {
        WalletServiceSettings {
            known_keys: self.user.known_keys,
            voucher_master_key_id: self.user.voucher_master_key_id,
            recovery_data,
            pending_note_expiry_blocks: self.user.pending_note_expiry_blocks,
            era_schedule: eras.map(|era| {
                let EraRuleset::V1(parameters) = &era.parameters.ruleset;
                WalletEraSettings {
                    epoch_length: era.parameters.epoch_length(),
                    security_param: parameters.cryptarchia.security_param,
                }
            }),
        }
    }
}
