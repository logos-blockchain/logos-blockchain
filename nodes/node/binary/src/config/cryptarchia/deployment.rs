use core::num::{NonZero, NonZeroU32};
use std::collections::HashMap;

use lb_core::{
    block::BlockNumber,
    mantle::genesis_tx::GenesisTx,
    sdp::{MinStake, ServiceType},
};
use lb_cryptarchia_engine::Config as ConsensusConfig;
use lb_pol::slot_activation_coefficient;
use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub epoch_config: EpochConfig,
    pub security_param: NonZeroU32,
    pub sdp_config: SdpConfig,
    pub gossipsub_protocol: String,
    pub genesis_state: GenesisTx,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochConfig {
    // The stake distribution is always taken at the beginning of the previous epoch.
    // This parameters controls how many slots to wait for it to be stabilized
    // The value is computed as epoch_stake_distribution_stabilization * int(floor(k / f))
    pub epoch_stake_distribution_stabilization: NonZero<u8>,
    // This parameter controls how many slots we wait after the stake distribution
    // snapshot has stabilized to take the nonce snapshot.
    pub epoch_period_nonce_buffer: NonZero<u8>,
    // This parameter controls how many slots we wait for the nonce snapshot to be considered
    // stabilized
    pub epoch_period_nonce_stabilization: NonZero<u8>,
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
    pub service_params: HashMap<ServiceType, ServiceParameters>,
    pub min_stake: MinStake,
}

// The same as `lb_core::sdp::ServiceParameters`, minus the
// `session_duration` values which are calculated from the other values
// provided.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServiceParameters {
    pub lock_period: u64,
    pub inactivity_period: u64,
    pub retention_period: u64,
    pub timestamp: BlockNumber,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

fn devnet_settings() -> Settings {
    Settings {
        epoch_config: EpochConfig {
            epoch_period_nonce_buffer: 3.try_into().unwrap(),
            epoch_period_nonce_stabilization: 4.try_into().unwrap(),
            epoch_stake_distribution_stabilization: 3.try_into().unwrap(),
        },
        gossipsub_protocol: "/logos-blockchain-devnet/cryptarchia/1.0.0".to_owned(),
        sdp_config: SdpConfig {
            min_stake: MinStake {
                threshold: 1,
                timestamp: 0,
            },
            service_params: [(
                ServiceType::BlendNetwork,
                ServiceParameters {
                    inactivity_period: 1,
                    lock_period: 10,
                    retention_period: 1,
                    timestamp: 0,
                },
            )]
            .into(),
        },
        security_param: 20.try_into().unwrap(),
        // TODO: Change this once the devnet genesis state is finalized.
        genesis_state: GenesisTx::new_mocked(),
    }
}
