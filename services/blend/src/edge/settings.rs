use core::num::NonZeroU64;

use lb_key_management_system_service::{backend::preload::KeyId, keys::UnsecuredEd25519Key};

use crate::{core::settings::CoverTrafficSettings, settings::TimingSettings};

#[derive(Clone, Debug)]
pub struct StartingBlendConfig<BackendSettings> {
    pub backend: BackendSettings,
    pub time: TimingSettings,
    pub non_ephemeral_signing_key_id: KeyId,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
    /// `R_c`: replication factor for data messages.
    pub data_replication_factor: u64,
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
    pub data_replication_factor: u64,
}

impl<BackendSettings> RunningBlendConfig<BackendSettings> {
    pub const fn session_leadership_quota(&self) -> u64 {
        let num_blend_layers = self.num_blend_layers.get();
        let additional_encapsulations = num_blend_layers
            .checked_mul(self.data_replication_factor)
            .expect("Overflow when computing total replication factor.");
        num_blend_layers
            .checked_add(additional_encapsulations)
            .expect("Overflow when computing leadership quota.")
    }
}
