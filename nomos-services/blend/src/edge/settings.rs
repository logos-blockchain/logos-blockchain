use core::num::NonZeroU64;

use key_management_system_service::{backend::preload::KeyId, keys::UnsecuredEd25519Key};

use crate::{core::settings::CoverTrafficSettings, settings::TimingSettings};

#[derive(Clone, Debug)]
pub struct StartingBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub time: TimingSettings,
    pub non_ephemeral_signing_key_id: KeyId,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
}

/// Same values as [`StartingBlendConfig`] but with the secret key exfiltrated
/// from the KMS.
#[derive(Clone)]
pub struct RunningBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub time: TimingSettings,
    pub non_ephemeral_signing_key: UnsecuredEd25519Key,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
}
