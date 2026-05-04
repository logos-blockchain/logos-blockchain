use lb_core::mantle::GenesisTx as _;
use lb_cryptarchia_engine::{EpochConfig, time::SlotConfig};
use lb_time_service::{
    TimeServiceSettings,
    backends::{NtpTimeBackendSettings, ntp::async_client::NTPClientSettings},
};

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
                genesis_time: cryptarchia_deployment
                    .genesis_block
                    .genesis_tx()
                    .cryptarchia_parameter()
                    .genesis_time,
            },
            epoch_config: EpochConfig {
                epoch_period_nonce_buffer: cryptarchia_deployment
                    .epoch_config
                    .epoch_period_nonce_buffer,
                epoch_stake_distribution_stabilization: cryptarchia_deployment
                    .epoch_config
                    .epoch_stake_distribution_stabilization,
                epoch_period_nonce_stabilization: cryptarchia_deployment
                    .epoch_config
                    .epoch_period_nonce_stabilization,
            },
            base_period_length: cryptarchia_deployment
                .consensus_config()
                .base_period_length(),
            backend: NtpTimeBackendSettings {
                ntp_client_settings: NTPClientSettings {
                    timeout: self.user.backend.client.timeout,
                    listening_interface: self.user.backend.client.listening_interface,
                },
                ntp_server: self.user.backend.server,
                update_interval: self.user.backend.update_interval,
            },
        }
    }
}
