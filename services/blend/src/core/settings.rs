use core::time::Duration;
use std::num::NonZeroU64;

use lb_core::blend::core_quota;
use lb_cryptarchia_engine::EraSchedule;
use lb_key_management_system_service::backend::preload::KeyId;
use lb_poq::Quota;
use lb_services_utils::overwatch::{RecoveryData, StorageRecoverySettings};
use lb_utils::math::PositiveF64;

use crate::settings::{EraTimingSettings, max_data_message_delay_in_rounds};

#[derive(Clone, Debug)]
pub struct CoreServiceSettings<BackendSettings, DispatcherSettings> {
    pub non_ephemeral_signing_key_id: KeyId,
    pub zk: ZkSettings,
    pub abstain_on_failure: bool,
    pub backend: BackendSettings,
    pub dispatcher: DispatcherSettings,
    pub era_schedule: EraSchedule<CoreServiceEraSettings>,
    pub recovery_data: RecoveryData,
}

/// The core service's settings in an era.
#[derive(Clone, Debug)]
pub struct CoreServiceEraSettings {
    pub scheduler: EraSchedulerSettings,
    pub time: EraTimingSettings,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub data_replication_factor: u64,
    pub activity_threshold_sensitivity: u64,
}

impl CoreServiceEraSettings {
    #[must_use]
    pub fn epoch_core_quota(&self, membership_size: usize) -> Quota {
        self.scheduler
            .cover
            .epoch_core_quota(self.num_blend_layers, &self.time, membership_size)
    }

    #[must_use]
    pub fn epoch_pow_quota(&self) -> Quota {
        self.num_blend_layers
            .get()
            .try_into()
            .expect("PoW Quota must fit within the width the `PoQ` circuit allows.")
    }

    #[must_use]
    pub const fn epoch_leadership_quota(&self) -> Quota {
        let num_blend_layers = self.num_blend_layers.get();
        let additional_encapsulations = num_blend_layers
            .checked_mul(self.data_replication_factor)
            .expect("Overflow when computing total replication factor.");
        let quota = num_blend_layers
            .checked_add(additional_encapsulations)
            .expect("Overflow when computing leadership quota.");
        match Quota::try_new(quota) {
            Ok(quota) => quota,
            Err(_) => {
                // Not using `expect` to keep the function a `const`, so this panics at compile
                // time if called in a const context.
                panic!("Leadership Quota must fit within the width the `PoQ` circuit allows.")
            }
        }
    }

    pub(super) const fn scheduler_settings(
        &self,
    ) -> lb_blend::scheduling::message_scheduler::Settings {
        lb_blend::scheduling::message_scheduler::Settings {
            maximum_release_delay_in_rounds: self.scheduler.delayer.maximum_release_delay_in_rounds,
            round_duration: Duration::from_secs(self.time.round_duration_in_seconds.get()),
            rounds_per_epoch: self.time.rounds_per_epoch,
            num_blend_layers: self.num_blend_layers,
        }
    }

    #[must_use]
    pub const fn max_data_message_delay_in_rounds(&self) -> NonZeroU64 {
        max_data_message_delay_in_rounds(
            self.num_blend_layers,
            self.scheduler.delayer.maximum_release_delay_in_rounds,
            self.time.network_absorption_in_rounds,
        )
    }

    /// `T_M` as a duration: how long the network has to deliver a data
    /// message released under these settings.
    #[must_use]
    pub const fn max_data_message_delay(&self) -> Duration {
        self.time
            .duration_of(self.max_data_message_delay_in_rounds().get())
    }
}

impl<BackendSettings, DispatcherSettings> StorageRecoverySettings
    for CoreServiceSettings<BackendSettings, DispatcherSettings>
{
    const RECOVERY_KEY_SUFFIX: &'static [u8] = b"blend/core";

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}

#[derive(Clone, Debug)]
pub struct EraSchedulerSettings {
    pub cover: EraCoverTrafficSettings,
    pub delayer: EraMessageDelayerSettings,
}

#[derive(Clone, Debug)]
pub struct EraCoverTrafficSettings {
    /// `F_c`: frequency at which cover messages are generated per round.
    pub message_frequency_per_round: PositiveF64,
}

#[cfg(test)]
impl Default for EraCoverTrafficSettings {
    fn default() -> Self {
        Self {
            message_frequency_per_round: 1.try_into().unwrap(),
        }
    }
}

impl EraCoverTrafficSettings {
    #[must_use]
    pub(crate) fn epoch_core_quota(
        &self,
        num_blend_layers: NonZeroU64,
        timings: &EraTimingSettings,
        membership_size: usize,
    ) -> Quota {
        core_quota(
            timings.rounds_per_epoch,
            self.message_frequency_per_round,
            num_blend_layers,
            membership_size,
        )
    }
}

#[derive(Clone, Debug)]
pub struct EraMessageDelayerSettings {
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[derive(Clone, Debug)]
pub struct ZkSettings {
    pub secret_key_kms_id: KeyId,
}
