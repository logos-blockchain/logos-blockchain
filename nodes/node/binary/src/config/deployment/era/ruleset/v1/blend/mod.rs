use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    ops::RangeInclusive,
    time::Duration,
};

use lb_binary_codec::canonical::BinaryEncode;
use lb_blend_service::settings::EraTimingSettings;
use lb_core::blend::core_quota;
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_utils::math::PositiveF64;
use nutype::nutype;
use serde::{Deserialize, Serialize};

use super::{
    cryptarchia::Settings as CryptarchiaDeploymentSettings,
    time::Settings as TimeDeploymentSettings,
};
use crate::config::deployment::{InvalidDeployment, ensure};

pub(crate) mod codec;

/// The peering degrees `Φ_CC` the Blend spec allows: below 3, a node opens no
/// connection of its own; above 5, nodes must open more connections than nodes
/// can accept, and dials are refused.
const TARGET_PEERING_DEGREE: RangeInclusive<u32> = 3..=5;

/// Deployment-specific Blend settings.
#[derive(Serialize, Deserialize, Debug, Clone, BinaryEncode)]
pub struct Settings {
    pub common: CommonSettings,
    pub core: CoreSettings,
}

impl Settings {
    /// Checks the settings an era runs Blend with, in epochs of
    /// `slots_per_epoch` slots of `slot_duration`, refusing the first value
    /// found invalid.
    pub(super) fn validate(
        &self,
        slots_per_epoch: u64,
        slot_duration: &Duration,
    ) -> Result<(), InvalidDeployment> {
        let degree = self.core.target_peering_degree.get();
        ensure!(
            TARGET_PEERING_DEGREE.contains(&degree),
            "The Blend peering degree, {degree}, must be in {TARGET_PEERING_DEGREE:?}."
        );
        // Computed now rather than once the era runs. A share `r₁` of at least
        // one message leaves room for an edge connection too:
        // `r_E = 2V/3 − Φ_CC · r₁ ≥ r₁`.
        self.try_connection_share_per_round()?;
        self.try_edge_node_connection_timeout(slot_duration)?;
        // Computed now rather than when an epoch starts, where it would stop
        // every node. The smallest network Blend runs on gives the largest quota.
        let minimum_network_size = NonZeroU64::from(self.common.minimum_network_size).get();
        core_quota(
            self.rounds_per_epoch(slots_per_epoch, slot_duration),
            self.core.scheduler.cover.message_frequency_per_round,
            self.common.num_blend_layers,
            self.core.scheduler.cover.replication_factor.get(),
            minimum_network_size as usize,
        )
        .map_err(|error| {
            InvalidDeployment::new(format!(
                "The core quota of a network of {minimum_network_size} nodes must fit the `PoQ` \
                 circuit: {error}."
            ))
        })?;
        Ok(())
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

    /// `W = ⌈M_W / (F_C · (1 + R_C) · β_C)⌉`: the observation window, the
    /// rounds in which a core connection carries `M_W` messages from cover
    /// traffic alone.
    ///
    /// `M_W = λ · (Φ_CC − 1) / (Φ_CC − 2)`, `λ` being
    /// [`CoreSettings::expected_deliveries_per_observation_window`]. A
    /// neighbour sends a node every message but the ones it first received
    /// from that node, so one with the fewest connections, `Φ_CC − 1`, each as
    /// likely to bring a message first, delivers `(Φ_CC − 2) / (Φ_CC − 1)` of
    /// them. Only `W` is rounded. The deployment checks that `Φ_CC` is at
    /// least 3 when it loads, so `Φ_CC − 2` is never zero.
    #[must_use]
    pub const fn rounds_per_observation_window(&self) -> NonZeroU128 {
        let peering_degree = self.core.target_peering_degree.get() as f64;
        let cover = &self.core.scheduler.cover;
        let cover_messages_per_round = cover.message_frequency_per_round.get()
            * (1.0 + cover.replication_factor.get() as f64)
            * self.common.num_blend_layers.get() as f64;
        let expected_deliveries = self.core.expected_deliveries_per_observation_window.get() as f64;
        let rounds = expected_deliveries * (peering_degree - 1.0)
            / ((peering_degree - 2.0) * cover_messages_per_round);
        NonZeroU128::new(rounds.ceil() as u128).expect(
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
        self.try_connection_share_per_round()
            .expect("Checked when the deployment loads.")
    }

    /// [`Self::connection_share_per_round`], or why the era cannot run with it.
    fn try_connection_share_per_round(&self) -> Result<NonZeroU64, InvalidDeployment> {
        let readable_per_round = 2 * u64::from(self.core.verification_rate_per_second.get()) / 3;
        let shares = u64::from(self.core.target_peering_degree.get()) + 1;
        NonZeroU64::new(readable_per_round / shares).ok_or_else(|| {
            InvalidDeployment::new(
                "The verification rate must allow at least one message per connection per round."
                    .to_owned(),
            )
        })
    }

    /// `r_E = 2V/3 − Φ_CC · r₁`: the edge connections a node accepts in a
    /// round.
    #[must_use]
    pub fn accepted_edge_connections_per_round(&self) -> NonZeroU64 {
        let readable_per_round = 2 * u64::from(self.core.verification_rate_per_second.get()) / 3;
        let taken_by_core_connections = u64::from(self.core.target_peering_degree.get())
            * self.connection_share_per_round().get();
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
        self.try_edge_node_connection_timeout(slot_duration)
            .expect("Checked when the deployment loads.")
    }

    /// [`Self::edge_node_connection_timeout`], or why the era cannot run with
    /// it.
    fn try_edge_node_connection_timeout(
        &self,
        slot_duration: &Duration,
    ) -> Result<Duration, InvalidDeployment> {
        let deadline_in_rounds = self.core.edge_node_send_deadline_in_rounds.get();
        let rounds = u32::try_from(deadline_in_rounds).map_err(|_| {
            InvalidDeployment::new(format!(
                "`T_E` must fit in a `u32` number of rounds, not {deadline_in_rounds}."
            ))
        })?;
        self.round_duration(slot_duration)
            .checked_mul(rounds)
            .ok_or_else(|| {
                InvalidDeployment::new(format!(
                    "The edge connection timeout of {deadline_in_rounds} rounds overflows a \
                     `Duration`."
                ))
            })
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
            cover_replication_factor: self.core.scheduler.cover.replication_factor.get(),
            minimum_network_size: self.common.minimum_network_size.into(),
            num_blend_layers: self.common.num_blend_layers,
            rounds_per_epoch: self.rounds_per_epoch(
                cryptarchia_deployment.slots_per_epoch(),
                &time_deployment.slot_duration(),
            ),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, BinaryEncode)]
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

#[derive(Serialize, Deserialize, Debug, Clone, BinaryEncode)]
pub struct CoreSettings {
    pub scheduler: SchedulerSettings,
    /// `Φ_CC`: the peering degree a core node maintains with other core nodes.
    pub target_peering_degree: NonZeroU32,
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
    /// `λ`: the messages a neighbour is expected to deliver to a node in one
    /// observation window, which sizes the window with the peering degree and
    /// the cover traffic (see [`Settings::rounds_per_observation_window`]).
    pub expected_deliveries_per_observation_window: NonZeroU64,
    pub activity_threshold_sensitivity: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, BinaryEncode)]
pub struct SchedulerSettings {
    pub cover: CoverTrafficSettings,
    pub delayer: MessageDelayerSettings,
}

#[derive(Serialize, Deserialize, Clone, Debug, BinaryEncode)]
pub struct CoverTrafficSettings {
    /// `F_c`: frequency at which cover messages are generated per round.
    pub message_frequency_per_round: PositiveF64,
    /// `R_c`: the redundancy of cover messages. A core node's quota, and so the
    /// cover messages it sends, grow to `1 + R_c` times what `F_c` gives.
    pub replication_factor: CoverReplicationFactor,
}

/// `R_c`: the redundancy of cover messages, which is 0. Nothing in the code
/// currently replicates a cover message, so a factor would only raise the
/// quota.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, BinaryEncode)]
#[serde(try_from = "u64", into = "u64")]
pub struct CoverReplicationFactor(u64);

impl CoverReplicationFactor {
    /// `factor`, if it is 0.
    #[must_use]
    pub const fn new(factor: u64) -> Option<Self> {
        if factor == 0 {
            Some(Self(factor))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for CoverReplicationFactor {
    type Error = String;

    fn try_from(factor: u64) -> Result<Self, Self::Error> {
        Self::new(factor).ok_or_else(|| {
            format!(
                "The cover replication factor, {factor}, must be 0: cover messages are not \
                 replicated."
            )
        })
    }
}

impl From<CoverReplicationFactor> for u64 {
    fn from(factor: CoverReplicationFactor) -> Self {
        factor.0
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, BinaryEncode)]
pub struct MessageDelayerSettings {
    /// ∆max: maximal delay time between two release rounds.
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU32, NonZeroU64};

    use lb_utils::math::PositiveF64;

    use super::{CoverReplicationFactor, Settings};
    use crate::config::DeploymentSettings;

    /// The default deployment's Blend settings, with `λ`, `Φ_CC`, `F_C` and
    /// `β_C` set to the values given, which are all the window depends on.
    fn with_window_inputs(
        expected_deliveries_per_observation_window: u64,
        target_peering_degree: u32,
        message_frequency_per_round: f64,
        num_blend_layers: u64,
    ) -> Settings {
        let mut settings = DeploymentSettings::default()
            .genesis_era_parameters()
            .blend
            .clone();
        settings.core.expected_deliveries_per_observation_window =
            NonZeroU64::new(expected_deliveries_per_observation_window).unwrap();
        settings.core.target_peering_degree = NonZeroU32::new(target_peering_degree).unwrap();
        settings.core.scheduler.cover.message_frequency_per_round =
            PositiveF64::try_from(message_frequency_per_round).unwrap();
        settings.common.num_blend_layers = NonZeroU64::new(num_blend_layers).unwrap();
        settings
    }

    /// The spec's 60 deliveries at a peering degree of 4, `F_C = 1` and
    /// `β_C = 3`: 90 messages, in the 30 rounds the window had when the spec
    /// fixed it at 90 messages.
    #[test]
    fn the_window_is_30_rounds_at_the_reference_parameters() {
        assert_eq!(
            with_window_inputs(60, 4, 1.0, 3)
                .rounds_per_observation_window()
                .get(),
            30
        );
    }

    /// The spec's 60 deliveries at a peering degree of 4 on one blending
    /// layer: the 90 messages take 90 rounds.
    #[test]
    fn the_window_is_90_rounds_at_the_testnet_parameters() {
        assert_eq!(
            with_window_inputs(60, 4, 1.0, 1)
                .rounds_per_observation_window()
                .get(),
            90
        );
    }

    /// At a peering degree of 3, a neighbour with the fewest connections, two,
    /// delivers half of the messages, so the window carries 120 of them.
    #[test]
    fn the_window_is_40_rounds_at_a_peering_degree_of_3() {
        assert_eq!(
            with_window_inputs(60, 3, 1.0, 3)
                .rounds_per_observation_window()
                .get(),
            40
        );
    }

    /// At a peering degree of 5, the window carries 80 messages, which take
    /// 26⅔ rounds. A window that falls between two rounds takes the later one,
    /// so a connection still carries at least the messages asked for in it.
    #[test]
    fn a_window_between_two_rounds_rounds_up() {
        assert_eq!(
            with_window_inputs(60, 5, 1.0, 3)
                .rounds_per_observation_window()
                .get(),
            27
        );
    }

    #[test]
    fn a_cover_replication_factor_does_not_deserialize() {
        let error = serde_yaml::from_str::<CoverReplicationFactor>("1")
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("The cover replication factor, 1, must be 0"),
            "{error}"
        );
    }
}
