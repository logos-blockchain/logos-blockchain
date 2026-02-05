use core::num::NonZeroU64;

use lb_blend_service::{core::settings::SchedulerSettings, settings::TimingSettings};
use lb_libp2p::protocol_name::StreamProtocol;
use lb_utils::math::NonNegativeF64;
use serde::{Deserialize, Serialize};

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
