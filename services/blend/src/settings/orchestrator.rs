use core::{num::NonZeroU64, time::Duration};

use lb_cryptarchia_engine::EraSchedule;
use lb_key_management_system_service::backend::preload::KeyId;

#[derive(Clone, Debug)]
pub struct OrchestratorSettings {
    /// The non-ephemeral signing key (NSK) corresponding to the public key
    /// registered in the membership (SDP).
    pub non_ephemeral_signing_key_id: KeyId,
    /// The key of the ZK identity this node declares with SDP.
    pub zk_secret_key_kms_id: KeyId,
    pub era_schedule: EraSchedule<EraOrchestratorSettings>,
}

#[derive(Clone, Debug)]
pub struct EraOrchestratorSettings {
    pub minimum_network_size: NonZeroU64,
    pub epoch_transition_period: Duration,
}
