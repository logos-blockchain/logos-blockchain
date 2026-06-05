use core::{num::NonZeroU64, time::Duration};

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::NonNegativeF64;
use nutype::nutype;
use serde::{Deserialize, Serialize};

use crate::config::{
    cryptarchia::deployment::Settings as CryptarchiaDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};

/// Deployment-specific Blend settings.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub common: CommonSettings,
    pub core: CoreSettings,
}

impl Settings {
    #[must_use]
    pub const fn round_duration(&self, slot_duration: &Duration) -> Duration {
        *slot_duration
    }

    /// Number of rounds per epoch, calculated as the number of slots per
    /// epoch, correctly scaled to account for the slot/round ratio.
    #[must_use]
    pub fn rounds_per_epoch(&self, slots_per_epoch: u64, slot_duration: &Duration) -> NonZeroU64 {
        ((slots_per_epoch * slot_duration.as_secs()) / self.round_duration(slot_duration).as_secs())
            .try_into()
            .expect("There must be at least one round per epoch.")
    }

    /// Number of rounds per observation window.
    ///
    /// The Blend spec defines this as `10 * ∆max`, where `∆max` is the maximal
    /// delay time between two release rounds.
    #[must_use]
    pub const fn rounds_per_observation_window(&self) -> NonZeroU64 {
        // TODO: Is `10` fixed or can it be derived from some other value?
        NonZeroU64::new(
            10 * self
                .core
                .scheduler
                .delayer
                .maximum_release_delay_in_rounds
                .get(),
        )
        .unwrap()
    }

    /// Duration of the epoch transition period.
    ///
    /// The Blend spec defines this as roughly the same time it takes to propose
    /// a new block.
    #[must_use]
    pub const fn epoch_transition(
        &self,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> Duration {
        Duration::from_secs(slot_duration.as_secs() * slots_per_block)
    }

    #[must_use]
    pub fn rewards_params(
        &self,
        cryptarchia_deployment: &CryptarchiaDeploymentSettings,
        time_deployment: &TimeDeploymentSettings,
    ) -> RewardsParameters {
        RewardsParameters {
            activity_threshold_sensitivity: self.core.activity_threshold_sensitivity,
            data_replication_factor: self.common.data_replication_factor,
            message_frequency_per_round: self.core.scheduler.cover.message_frequency_per_round,
            minimum_network_size: self.common.minimum_network_size.into(),
            num_blend_layers: self.common.num_blend_layers,
            rounds_per_epoch: self.rounds_per_epoch(
                cryptarchia_deployment.slots_per_epoch(),
                &time_deployment.slot_duration,
            ),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommonSettings {
    /// `ß_c`: expected number of blending operations for each locally generated
    /// message.
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: MinimumNetworkSize,
    pub protocol_name: StreamProtocol,
    pub data_replication_factor: u64,
}

#[nutype(
    validate(greater_or_equal = 2),
    derive(Serialize, Deserialize, Debug, Clone, Copy)
)]
pub struct MinimumNetworkSize(u64);

impl From<MinimumNetworkSize> for NonZeroU64 {
    fn from(value: MinimumNetworkSize) -> Self {
        value
            .into_inner()
            .try_into()
            .expect("Minimum network size is at least 2, which is > than 0.")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoreSettings {
    pub scheduler: SchedulerSettings,
    // TODO: Can we derive this?
    pub minimum_messages_coefficient: NonZeroU64,
    pub normalization_constant: NonNegativeF64,
    pub activity_threshold_sensitivity: u64,
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
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageDelayerSettings {
    /// ∆max: maximal delay time between two release rounds.
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use core::{num::NonZeroU64, time::Duration};

    use crate::config::{DeploymentSettings, WellKnownDeployment};

    #[test]
    fn blend_devnet() {
        const EXPECTED_ROUND_DURATION: Duration = Duration::from_secs(1);
        const EXPECTED_ROUNDS_PER_EPOCH: NonZeroU64 = NonZeroU64::new(6_000).unwrap();
        const EXPECTED_ROUNDS_PER_OBSERVATION_WINDOW: NonZeroU64 = NonZeroU64::new(10).unwrap();
        const EXPECTED_EPOCH_TRANSITION_PERIOD: Duration = Duration::from_secs(20);

        let deployment: DeploymentSettings = WellKnownDeployment::Devnet.into();

        let slots_per_epoch = deployment.cryptarchia.slots_per_epoch();
        let slots_per_block = deployment.cryptarchia.average_slots_per_block();
        let slot_duration = deployment.time.slot_duration;

        assert_eq!(deployment.blend_round_duration(), EXPECTED_ROUND_DURATION);

        assert_eq!(
            deployment
                .blend
                .rounds_per_epoch(slots_per_epoch, &slot_duration),
            EXPECTED_ROUNDS_PER_EPOCH
        );

        assert_eq!(
            deployment.blend.rounds_per_observation_window(),
            EXPECTED_ROUNDS_PER_OBSERVATION_WINDOW
        );

        assert_eq!(
            deployment
                .blend
                .epoch_transition(slots_per_block, &slot_duration),
            EXPECTED_EPOCH_TRANSITION_PERIOD
        );
    }
}
