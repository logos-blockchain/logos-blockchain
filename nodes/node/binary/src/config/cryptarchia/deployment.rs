use core::num::NonZeroU32;
use std::collections::HashMap;

use lb_core::{
    block::BlockNumber,
    sdp::{MinStake, ServiceType},
};
use lb_cryptarchia_engine::{Config as ConsensusConfig, EpochConfig};
use lb_pol::slot_activation_coefficient;
use serde::{Deserialize, Serialize};

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
