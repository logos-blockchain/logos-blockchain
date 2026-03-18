use core::{num::NonZeroU64, time::Duration};

use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::NonNegativeF64;
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

    /// Number of rounds per session, calculated as the number of slots per
    /// epoch, correctly scaled to account for the slot/round ratio.
    #[must_use]
    pub fn rounds_per_session(&self, slots_per_epoch: u64, slot_duration: &Duration) -> NonZeroU64 {
        ((slots_per_epoch * slot_duration.as_secs()) / self.round_duration(slot_duration).as_secs())
            .try_into()
            .expect("There must be at least one round per session.")
    }

    /// Number of rounds per interval, calculated as the average number of slots
    /// per block (slot activation threshold), correctly scaled to account for
    /// the slot/round ratio.
    #[must_use]
    pub fn rounds_per_interval(
        &self,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> NonZeroU64 {
        ((slots_per_block * slot_duration.as_secs()) / self.round_duration(slot_duration).as_secs())
            .try_into()
            .expect("There must be at least one round per interval.")
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

    /// Number of rounds per session transition period.
    ///
    /// The Blend spec defines this as roughly the same as
    /// [`rounds_per_interval`].
    #[must_use]
    pub fn rounds_per_session_transition_period(
        &self,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> NonZeroU64 {
        self.rounds_per_interval(slots_per_block, slot_duration)
    }

    /// Number of rounds per epoch transition period.
    ///
    /// The Blend spec defines this as roughly the same time it takes to propose
    /// a new block.
    #[must_use]
    pub fn slots_per_epoch_transition_period(
        &self,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> NonZeroU64 {
        let rounds_per_session_transition_period =
            self.rounds_per_session_transition_period(slots_per_block, slot_duration);
        ((self.round_duration(slot_duration).as_secs()
            * rounds_per_session_transition_period.get())
            / slot_duration.as_secs())
        .try_into()
        .expect("There must be at least one slot per epoch transition period.")
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
            minimum_network_size: self.common.minimum_network_size,
            num_blend_layers: self.common.num_blend_layers,
            rounds_per_session: self.rounds_per_session(
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
    pub minimum_network_size: NonZeroU64,
    pub protocol_name: StreamProtocol,
    pub data_replication_factor: u64,
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
    // `max`: safety buffer length, expressed in intervals
    // TODO: Can we derive this?
    pub intervals_for_safety_buffer: u64,
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
        const EXPECTED_ROUNDS_PER_SESSION: NonZeroU64 = NonZeroU64::new(6_000).unwrap();
        const EXPECTED_ROUNDS_PER_INTERVAL: NonZeroU64 = NonZeroU64::new(20).unwrap();
        const EXPECTED_ROUNDS_PER_OBSERVATION_WINDOW: NonZeroU64 = NonZeroU64::new(10).unwrap();
        const EXPECTED_ROUNDS_PER_SESSION_TRANSITION_PERIOD: NonZeroU64 =
            NonZeroU64::new(20).unwrap();
        const EXPECTED_SLOTS_PER_EPOCH_TRANSITION_PERIOD: NonZeroU64 = NonZeroU64::new(20).unwrap();

        let deployment: DeploymentSettings = WellKnownDeployment::Devnet.into();

        let slots_per_epoch = deployment.cryptarchia.slots_per_epoch();
        let slots_per_block = deployment.cryptarchia.average_slots_per_block();
        let slot_duration = deployment.time.slot_duration;

        assert_eq!(deployment.blend_round_duration(), EXPECTED_ROUND_DURATION);

        assert_eq!(
            deployment
                .blend
                .rounds_per_session(slots_per_epoch, &slot_duration),
            EXPECTED_ROUNDS_PER_SESSION
        );

        assert_eq!(
            deployment
                .blend
                .rounds_per_interval(slots_per_block, &slot_duration),
            EXPECTED_ROUNDS_PER_INTERVAL
        );

        assert_eq!(
            deployment.blend.rounds_per_observation_window(),
            EXPECTED_ROUNDS_PER_OBSERVATION_WINDOW
        );

        assert_eq!(
            deployment
                .blend
                .rounds_per_session_transition_period(slots_per_block, &slot_duration),
            EXPECTED_ROUNDS_PER_SESSION_TRANSITION_PERIOD
        );

        assert_eq!(
            deployment
                .blend
                .slots_per_epoch_transition_period(slots_per_block, &slot_duration),
            EXPECTED_SLOTS_PER_EPOCH_TRANSITION_PERIOD
        );
    }
}
