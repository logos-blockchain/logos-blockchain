use std::{num::NonZeroU64, path::PathBuf};

use key_management_system_service::backend::preload::KeyId;
use nomos_blend::scheduling::message_blend::crypto::SessionCryptographicProcessorSettings;
use nomos_core::blend::core_quota;
use nomos_utils::math::NonNegativeF64;
use serde::{Deserialize, Serialize};
use services_utils::overwatch::recovery::backends::FileBackendSettings;

use crate::settings::TimingSettings;

#[derive(Clone, Debug)]
pub struct BlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub crypto: SessionCryptographicProcessorSettings,
    pub scheduler: SchedulerSettings,
    pub time: TimingSettings,
    pub zk: ZkSettings,
    pub minimum_network_size: NonZeroU64,
    pub recovery_path: PathBuf,
}

impl<BackendSettings> BlendConfig<BackendSettings> {
    pub fn session_quota(&self, membership_size: usize) -> u64 {
        self.scheduler
            .cover
            .session_quota(&self.crypto, &self.time, membership_size)
    }

    pub(super) fn scheduler_settings(
        &self,
    ) -> nomos_blend::scheduling::message_scheduler::Settings {
        nomos_blend::scheduling::message_scheduler::Settings {
            additional_safety_intervals: self.scheduler.cover.intervals_for_safety_buffer,
            expected_intervals_per_session: self.time.intervals_per_session(),
            maximum_release_delay_in_rounds: self.scheduler.delayer.maximum_release_delay_in_rounds,
            round_duration: self.time.round_duration,
            rounds_per_interval: self.time.rounds_per_interval,
            num_blend_layers: self.crypto.num_blend_layers,
        }
    }
}

impl<BackendSettings> FileBackendSettings for BlendConfig<BackendSettings> {
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
    pub(crate) fn session_quota(
        &self,
        crypto: &SessionCryptographicProcessorSettings,
        timings: &TimingSettings,
        membership_size: usize,
    ) -> u64 {
        core_quota(
            timings.rounds_per_session,
            self.message_frequency_per_round,
            crypto.num_blend_layers,
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
