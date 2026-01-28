use std::{num::NonZeroU64, path::PathBuf};

use lb_core::blend::core_quota;
use lb_key_management_system_service::{backend::preload::KeyId, keys::UnsecuredEd25519Key};
use lb_services_utils::overwatch::recovery::backends::FileBackendSettings;
use lb_utils::math::NonNegativeF64;
use serde::{Deserialize, Serialize};

use crate::settings::TimingSettings;

#[derive(Clone, Debug)]
pub struct StartingBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub scheduler: SchedulerSettings,
    pub time: TimingSettings,
    pub zk: ZkSettings,
    pub non_ephemeral_signing_key_id: KeyId,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub recovery_path: PathBuf,
    /// `R_c`: replication factor for data messages.
    pub data_replication_factor: u64,
}

/// Same values as [`StartingBlendConfig`] but with the secret key exfiltrated
/// from the KMS.
#[derive(Clone)]
pub struct RunningBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub scheduler: SchedulerSettings,
    pub time: TimingSettings,
    pub zk: ZkSettings,
    pub non_ephemeral_signing_key: UnsecuredEd25519Key,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub recovery_path: PathBuf,
    pub data_replication_factor: u64,
}

impl<BackendSettings> RunningBlendConfig<BackendSettings> {
    pub fn session_core_quota(&self, membership_size: usize) -> u64 {
        self.scheduler
            .cover
            .session_core_quota(self.num_blend_layers, &self.time, membership_size)
    }

    pub const fn session_leadership_quota(&self) -> u64 {
        let num_blend_layers = self.num_blend_layers.get();
        let additional_encapsulations = num_blend_layers
            .checked_mul(self.data_replication_factor)
            .expect("Overflow when computing total replication factor.");
        num_blend_layers
            .checked_add(additional_encapsulations)
            .expect("Overflow when computing leadership quota.")
    }

    pub(super) fn scheduler_settings(&self) -> lb_blend::scheduling::message_scheduler::Settings {
        lb_blend::scheduling::message_scheduler::Settings {
            additional_safety_intervals: self.scheduler.cover.intervals_for_safety_buffer,
            expected_intervals_per_session: self.time.intervals_per_session(),
            maximum_release_delay_in_rounds: self.scheduler.delayer.maximum_release_delay_in_rounds,
            round_duration: self.time.round_duration,
            rounds_per_interval: self.time.rounds_per_interval,
            num_blend_layers: self.num_blend_layers,
        }
    }
}

impl<BackendSettings> FileBackendSettings for StartingBlendConfig<BackendSettings> {
    fn recovery_file(&self) -> &PathBuf {
        &self.recovery_path
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SchedulerSettings {
    pub cover: CoverTrafficSettings,
    pub delayer: MessageDelayerSettings,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CoverTrafficSettings {
    /// `F_c`: frequency at which cover messages are generated per round.
    pub message_frequency_per_round: NonNegativeF64,
    // `max`: safety buffer length, expressed in intervals
    pub intervals_for_safety_buffer: u64,
}

#[cfg(test)]
impl Default for CoverTrafficSettings {
    fn default() -> Self {
        Self {
            intervals_for_safety_buffer: 1,
            message_frequency_per_round: 1.try_into().unwrap(),
        }
    }
}

impl CoverTrafficSettings {
    #[must_use]
    pub(crate) fn session_core_quota(
        &self,
        num_blend_layers: NonZeroU64,
        timings: &TimingSettings,
        membership_size: usize,
    ) -> u64 {
        core_quota(
            timings.rounds_per_session,
            self.message_frequency_per_round,
            num_blend_layers,
            membership_size,
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageDelayerSettings {
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ZkSettings {
    pub secret_key_kms_id: KeyId,
}
