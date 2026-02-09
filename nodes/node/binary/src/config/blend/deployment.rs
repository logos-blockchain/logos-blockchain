use core::{num::NonZeroU64, time::Duration};

use lb_blend_service::{
    core::settings::{CoverTrafficSettings, MessageDelayerSettings, SchedulerSettings},
    settings::TimingSettings,
};
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

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoreSettings {
    pub scheduler: SchedulerSettings,
    pub minimum_messages_coefficient: NonZeroU64,
    pub normalization_constant: NonNegativeF64,
    pub activity_threshold_sensitivity: u64,
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
