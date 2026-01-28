use core::num::NonZeroU32;
use std::{collections::HashMap, sync::Arc};

use lb_core::sdp::{MinStake, ServiceParameters, ServiceType};
use lb_cryptarchia_engine::{Config as ConsensusConfig, EpochConfig};
use lb_pol::slot_activation_coefficient;
use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub sdp_config: SdpConfig,
    pub gossipsub_protocol: String,
}

impl Settings {
    #[must_use]
    pub const fn consensus_config(&self) -> ConsensusConfig {
        ConsensusConfig::new(self.security_param, slot_activation_coefficient())
    }
}

// The same as `lb_ledger::mantle::sdp::Config`, minus the
// `service_rewards_params` values, which are taken from the Blend deployment
// config instead.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SdpConfig {
    pub service_params: Arc<HashMap<ServiceType, ServiceParameters>>,
    pub min_stake: MinStake,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Mainnet => mainnet_settings(),
            WellKnownDeployment::Testnet => testnet_settings(),
        }
    }
}

fn mainnet_settings() -> Settings {
    Settings {
        epoch_config: EpochConfig {
            epoch_period_nonce_buffer: 3.try_into().unwrap(),
            epoch_period_nonce_stabilization: 4.try_into().unwrap(),
            epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
        },
        security_param: 10.try_into().unwrap(),
        sdp_config: SdpConfig {
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
            service_params: Arc::new(
                std::iter::once((
                    ServiceType::BlendNetwork,
                    ServiceParameters {
                        inactivity_period: 20,
                        lock_period: 10,
                        retention_period: 100,
                        session_duration: 21_600,
                        timestamp: 0,
                    },
                ))
                .collect(),
            ),
        },
        gossipsub_protocol: "/logos-blockchain/cryptarchia/1.0.0".to_owned(),
    }
}

fn testnet_settings() -> Settings {
    mainnet_settings()
}
