use core::{num::NonZeroU64, time::Duration};

use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::NonNegativeF64;
use serde::{Deserialize, Serialize};

use crate::config::deployment::WellKnownDeployment;

/// Deployment-specific Blend settings.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Settings {
    pub common: CommonSettings,
    pub core: CoreSettings,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommonSettings {
    /// `ß_c`: expected number of blending operations for each locally generated
    /// message.
    pub num_blend_layers: NonZeroU64,
    pub timing: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    pub protocol_name: StreamProtocol,
    pub data_replication_factor: u64,
}

#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TimingSettings {
    /// `S`: length of a session in terms of expected rounds (on average).
    pub rounds_per_session: NonZeroU64,
    /// `|I|`: length of an interval in terms of rounds.
    pub rounds_per_interval: NonZeroU64,
    #[serde_as(
        as = "lb_utils::bounded_duration::MinimalBoundedDuration<1, lb_utils::bounded_duration::SECOND>"
    )]
    /// Duration of a round.
    pub round_duration: Duration,
    pub rounds_per_observation_window: NonZeroU64,
    /// Session transition period in rounds.
    pub rounds_per_session_transition_period: NonZeroU64,
    pub epoch_transition_period_in_slots: NonZeroU64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoreSettings {
    pub scheduler: SchedulerSettings,
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
    pub intervals_for_safety_buffer: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MessageDelayerSettings {
    pub maximum_release_delay_in_rounds: NonZeroU64,
}

impl From<WellKnownDeployment> for Settings {
    fn from(value: WellKnownDeployment) -> Self {
        match value {
            WellKnownDeployment::Devnet => devnet_settings(),
        }
    }
}

fn devnet_settings() -> Settings {
    Settings {
        common: CommonSettings {
            data_replication_factor: 0,
            minimum_network_size: 32.try_into().unwrap(),
            num_blend_layers: 3.try_into().unwrap(),
            protocol_name: StreamProtocol::new("/logos-blockchain-devnet/blend/1.0.0"),
            timing: TimingSettings {
                epoch_transition_period_in_slots: 20.try_into().unwrap(),
                round_duration: Duration::from_secs(1),
                rounds_per_interval: 10.try_into().unwrap(),
                rounds_per_observation_window: 20.try_into().unwrap(),
                rounds_per_session: 2_000.try_into().unwrap(),
                rounds_per_session_transition_period: 20.try_into().unwrap(),
            },
        },
        core: CoreSettings {
            activity_threshold_sensitivity: 1,
            minimum_messages_coefficient: 1.try_into().unwrap(),
            normalization_constant: 1.03.try_into().unwrap(),
            scheduler: SchedulerSettings {
                cover: CoverTrafficSettings {
                    intervals_for_safety_buffer: 100,
                    message_frequency_per_round: 1.0.try_into().unwrap(),
                },
                delayer: MessageDelayerSettings {
                    maximum_release_delay_in_rounds: 3.try_into().unwrap(),
                },
            },
        },
    }
}
