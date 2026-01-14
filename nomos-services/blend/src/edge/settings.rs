use core::num::NonZeroU64;

use crate::{
    core::settings::CoverTrafficSettings,
    settings::{
        InitializedSessionCryptographicProcessorSettings, SessionCryptographicProcessorSettings,
        TimingSettings,
    },
};

#[derive(Clone, Debug)]
pub struct BlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub crypto: SessionCryptographicProcessorSettings,
    pub time: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
}

#[derive(Clone)]
pub struct InitializedBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub crypto: InitializedSessionCryptographicProcessorSettings,
    pub time: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
}
