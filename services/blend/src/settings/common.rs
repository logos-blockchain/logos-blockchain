use core::num::NonZeroU64;
use std::path::PathBuf;

use lb_key_management_system_service::backend::preload::KeyId;
use serde::{Deserialize, Serialize};

use crate::settings::timing::TimingSettings;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommonSettings {
    /// The non-ephemeral signing key (NSK) corresponding to the public key
    /// registered in the membership (SDP).
    pub non_ephemeral_signing_key_id: KeyId,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
    pub time: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    pub recovery_path_prefix: PathBuf,
    pub data_replication_factor: u64,
}
