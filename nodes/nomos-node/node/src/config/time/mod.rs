use cryptarchia_engine::time::SlotConfig;
use nomos_time::{TimeServiceSettings, backends::NtpTimeBackendSettings};

use crate::config::{
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    time::{deployment::Settings as DeploymentSettings, serde::Config},
};

pub mod deployment;
pub mod serde;

pub struct ServiceConfig {
    pub user: Config,
    pub deployment: DeploymentSettings,
}

impl ServiceConfig {
    #[must_use]
    pub fn into_time_service_settings(
        self,
        cryptarchia_deployment: &CryptarchiaDeploymentSettings,
    ) -> TimeServiceSettings<NtpTimeBackendSettings> {
        TimeServiceSettings {
            slot_config: SlotConfig {
                slot_duration: self.deployment.slot_duration,
                chain_start_time: self.user.chain_start_time,
            },
            epoch_config: cryptarchia_deployment.epoch_config,
            base_period_length: cryptarchia_deployment.consensus_config.base_period_length(),
            backend: self.user.backend,
        }
    }
}
