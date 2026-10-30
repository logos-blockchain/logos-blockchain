use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    time::Duration,
};

use lb_blend_service::settings::EraTimingSettings;
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_utils::math::PositiveF64;
use nutype::nutype;
use serde::{Deserialize, Serialize};

use super::{
    cryptarchia::Settings as CryptarchiaDeploymentSettings,
    time::Settings as TimeDeploymentSettings,
};

pub(crate) mod codec;

/// `Φ_CC`: the peering degree a core node maintains with other core nodes.
///
/// The Blend spec sets it to 4, within the 3 to 5 it allows: below 3, a node
/// opens no connection of its own; above 5, nodes must open more connections
/// than nodes can accept, and dials are refused.
pub const TARGET_PEERING_DEGREE: NonZeroU32 = NonZeroU32::new(4).unwrap();

/// Deployment-specific Blend settings.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub common: CommonSettings,
    pub core: CoreSettings,
}

impl Settings {
    /// Checks the settings an era runs Blend with, in slots of `slot_duration`,
    /// panicking on the first value found invalid.
    pub(super) fn validate(&self, slot_duration: &Duration) {
        // Each panics on values the node cannot run with: computed now rather
        // than once the era runs. A share `r₁` of at least one message leaves
        // room for an edge connection too: `r_E = 2V/3 − Φ_CC · r₁ ≥ r₁`.
        let _ = self.connection_share_per_round();
        let _ = self.edge_node_connection_timeout(slot_duration);
    }

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

    /// `W = ⌈M / (F_C · (1 + R_C) · β_C)⌉`: the observation window, the rounds
    /// in which a core connection carries `M` messages from cover traffic
    /// alone, `M` being
    /// [`CoreSettings::cover_messages_per_observation_window`].
    ///
    /// With fewer messages in the window, a node more often reads an honest
    /// neighbour as not live and closes its connection. No deployment
    /// replicates cover messages, so `R_C` is `0`.
    #[must_use]
    pub const fn rounds_per_observation_window(&self) -> NonZeroU128 {
        let cover_messages_per_round = self.core.scheduler.cover.message_frequency_per_round.get()
            * self.common.num_blend_layers.get() as f64;
        let cover_messages_per_window =
            self.core.cover_messages_per_observation_window.get() as f64;
        NonZeroU128::new((cover_messages_per_window / cover_messages_per_round).ceil() as u128)
        .expect(
            "The cover traffic of a round must be finite, so the window spans at least one round.",
        )
    }

    /// `r₁ = ⌊(2V/3) / (Φ_CC + 1)⌋`: the messages a core connection may carry
    /// in one round, in each direction.
    ///
    /// Two thirds of the verification rate is what a node at its peering degree
    /// reads, divided evenly between the connections it may hold and the edge
    /// nodes it serves.
    #[must_use]
    pub fn connection_share_per_round(&self) -> NonZeroU64 {
        let readable_per_round = 2 * u64::from(self.core.verification_rate_per_second.get()) / 3;
        let shares = u64::from(TARGET_PEERING_DEGREE.get()) + 1;
        NonZeroU64::new(readable_per_round / shares).expect(
            "The verification rate must allow at least one message per connection per round.",
        )
    }

    /// `r_E = 2V/3 − Φ_CC · r₁`: the edge connections a node accepts in a
    /// round.
    #[must_use]
    pub fn accepted_edge_connections_per_round(&self) -> NonZeroU64 {
        let readable_per_round = 2 * u64::from(self.core.verification_rate_per_second.get()) / 3;
        let taken_by_core_connections =
            u64::from(TARGET_PEERING_DEGREE.get()) * self.connection_share_per_round().get();
        NonZeroU64::new(readable_per_round.saturating_sub(taken_by_core_connections)).expect(
            "The verification rate must leave room for at least one edge connection per round.",
        )
    }

    /// `Φ_CE^Max`: the value the spec puts on how many edge connections a
    /// node holds at once, `2·r_E`.
    #[must_use]
    pub fn maximum_concurrent_edge_connections(&self) -> NonZeroU64 {
        self.accepted_edge_connections_per_round()
            .checked_mul(NonZeroU64::new(2).unwrap())
            .expect("The maximum number of concurrent edge connections overflowed `u64`.")
    }

    #[must_use]
    pub fn edge_node_connection_timeout(&self, slot_duration: &Duration) -> Duration {
        self.round_duration(slot_duration)
            .checked_mul(
                self.core
                    .edge_node_send_deadline_in_rounds
                    .get()
                    .try_into()
                    .expect("`T_E` must fit in a `u32` number of rounds."),
            )
            .expect("The edge connection timeout overflowed a `Duration`.")
    }

    #[must_use]
    pub fn timing_settings(
        &self,
        slots_per_epoch: u64,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> EraTimingSettings {
        EraTimingSettings {
            epoch_transition_period: self.epoch_transition(slots_per_block, slot_duration),
            round_duration_in_seconds: self
                .round_duration(slot_duration)
                .as_secs()
                .try_into()
                .expect("Round duration must be greater than `0` seconds."),
            rounds_per_observation_window: self.rounds_per_observation_window(),
            network_absorption_in_rounds: self.common.network_absorption_in_rounds,
            core_handshake_deadline_in_rounds: self.core.core_handshake_deadline_in_rounds,
            rounds_per_epoch: self.rounds_per_epoch(slots_per_epoch, slot_duration),
        }
    }

    /// Duration of the epoch transition period.
    #[must_use]
    pub const fn epoch_transition(
        &self,
        slots_per_block: u64,
        slot_duration: &Duration,
    ) -> Duration {
        Duration::from_secs(slot_duration.as_secs() * self.epoch_transition_slots(slots_per_block))
    }

    /// Length of the epoch transition period, in slots.
    ///
    /// The Blend spec defines this as roughly the same time it takes to propose
    /// a new block.
    #[must_use]
    pub const fn epoch_transition_slots(&self, slots_per_block: u64) -> u64 {
        slots_per_block
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
                &time_deployment.slot_duration(),
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
    /// `η`: the network absorption of one hop, the rounds a message spends
    /// crossing the network between two blend nodes.
    pub network_absorption_in_rounds: NonZeroU64,
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
    /// `V`: the messages per second the slowest node the protocol targets can
    /// verify the public header of. Every admission share is sized so that what
    /// a node reads in a round stays within it.
    pub verification_rate_per_second: NonZeroU32,
    /// `T_E`: the rounds an edge node is given to send its message, counted
    /// from the moment its connection is accepted.
    pub edge_node_send_deadline_in_rounds: NonZeroU64,
    /// `T_H`: the rounds a handshake with a core node is given to complete,
    /// covering the round trips of the transport handshake and of the
    /// neighbour distinction process.
    pub core_handshake_deadline_in_rounds: NonZeroU128,
    /// The messages a core connection carries from cover traffic alone in one
    /// observation window, which sizes the window (see
    /// [`Settings::rounds_per_observation_window`]).
    // TODO: Is this rather derived from other deployment parameters?
    pub cover_messages_per_observation_window: NonZeroU64,
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
    pub message_frequency_per_round: PositiveF64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageDelayerSettings {
    /// ∆max: maximal delay time between two release rounds.
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use lb_utils::math::PositiveF64;

    use super::Settings;
    use crate::config::DeploymentSettings;

    /// The default deployment's Blend settings, with the cover messages per
    /// window, `F_C` and `β_C` set to the values given, which are all the
    /// window depends on.
    fn with_cover_traffic(
        cover_messages_per_observation_window: u64,
        message_frequency_per_round: f64,
        num_blend_layers: u64,
    ) -> Settings {
        let mut settings = DeploymentSettings::default()
            .genesis_era_parameters()
            .blend
            .clone();
        settings.core.cover_messages_per_observation_window =
            NonZeroU64::new(cover_messages_per_observation_window).unwrap();
        settings.core.scheduler.cover.message_frequency_per_round =
            PositiveF64::try_from(message_frequency_per_round).unwrap();
        settings.common.num_blend_layers = NonZeroU64::new(num_blend_layers).unwrap();
        settings
    }

    /// The spec's 90 messages at `F_C = 1` and `β_C = 3`, where the window is
    /// the 30 rounds that `10·Δ_max` gave it, so neither the liveness test nor
    /// the blacklist expiry changes.
    #[test]
    fn the_window_is_30_rounds_at_the_reference_parameters() {
        assert_eq!(
            with_cover_traffic(90, 1.0, 3)
                .rounds_per_observation_window()
                .get(),
            30
        );
    }

    /// The spec's 90 messages on one blending layer at `F_C = 1`, where
    /// `10·Δ_max` gave 10 rounds, in which an honest neighbour delivers about
    /// five messages.
    #[test]
    fn the_window_is_90_rounds_at_the_testnet_parameters() {
        assert_eq!(
            with_cover_traffic(90, 1.0, 1)
                .rounds_per_observation_window()
                .get(),
            90
        );
    }

    /// A window that falls between two rounds takes the later one, so a
    /// connection still carries at least the messages asked for in it.
    #[test]
    fn a_window_between_two_rounds_rounds_up() {
        assert_eq!(
            with_cover_traffic(100, 1.0, 3)
                .rounds_per_observation_window()
                .get(),
            34
        );
    }
}
