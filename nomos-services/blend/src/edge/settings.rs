use core::num::NonZeroU64;

use nomos_blend::scheduling::message_blend::crypto::SessionCryptographicProcessorSettings;

use crate::{core::settings::CoverTrafficSettings, settings::TimingSettings};

#[derive(Clone, Debug)]
pub struct BlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub crypto: SessionCryptographicProcessorSettings,
    pub time: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
}
