use std::{collections::HashMap, sync::Arc};

use cryptarchia_engine::{Config as ConsensusConfig, EpochConfig};
use nomos_core::sdp::{MinStake, ServiceParameters, ServiceType};
use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub consensus_config: ConsensusConfig,
    pub sdp_config: SdpConfig,
    pub gossipsub_protocol: String,
}

// The same as `nomos_ledger::mantle::sdp::Config`, minus the
// `service_rewards_params` values, which are taken from the Blend deployment
// config instead.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SdpConfig {
    pub service_params: Arc<HashMap<ServiceType, ServiceParameters>>,
    pub min_stake: MinStake,
}

#[expect(clippy::fallible_impl_from, reason = "Well-known values.")]
impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Mainnet => Self {
                epoch_config: EpochConfig {
                    epoch_period_nonce_buffer: 3.try_into().unwrap(),
                    epoch_period_nonce_stabilization: 4.try_into().unwrap(),
                    epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
                },
                consensus_config: ConsensusConfig {
                    active_slot_coeff: 0.9,
                    security_param: 10.try_into().unwrap(),
                },
                sdp_config: SdpConfig {
                    min_stake: MinStake {
                        threshold: 1,
                        timestamp: 0,
                    },
                    service_params: Arc::new(
                        [
                            (
                                ServiceType::BlendNetwork,
                                ServiceParameters {
                                    inactivity_period: 20,
                                    lock_period: 10,
                                    retention_period: 100,
                                    session_duration: 21_600,
                                    timestamp: 0,
                                },
                            ),
                            (
                                ServiceType::DataAvailability,
                                ServiceParameters {
                                    inactivity_period: 20,
                                    lock_period: 10,
                                    retention_period: 100,
                                    session_duration: 1_000,
                                    timestamp: 0,
                                },
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                },
                gossipsub_protocol: "/cryptarchia/proto".to_owned(),
            },
        }
    }
}
