use core::{
    num::{NonZeroU32, NonZeroU64, NonZeroU128},
    time::Duration,
};

use lb_blend_service::settings::TimingSettings;
use lb_ledger::mantle::sdp::rewards::blend::RewardsParameters;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::PositiveF64;
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
    pub const fn rounds_per_observation_window(&self) -> NonZeroU128 {
        // TODO: Is `10` fixed or can it be derived from some other value?
        NonZeroU128::new(
            10 * self
                .core
                .scheduler
                .delayer
                .maximum_release_delay_in_rounds
                .get() as u128,
        )
        .unwrap()
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
        let shares = u64::from(self.core.target_peering_degree.get()) + 1;
        NonZeroU64::new(readable_per_round / shares).expect(
            "The verification rate must allow at least one message per connection per round.",
        )
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
    ) -> TimingSettings {
        TimingSettings {
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
