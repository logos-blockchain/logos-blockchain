use core::num::NonZeroU64;

use lb_key_management_system_service::backend::preload::KeyId;
use lb_services_utils::overwatch::RecoveryData;
use serde::{Deserialize, Serialize};

use crate::settings::timing::TimingSettings;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommonSettings<BroadcastSettings> {
    /// The non-ephemeral signing key (NSK) corresponding to the public key
    /// registered in the membership (SDP).
    pub non_ephemeral_signing_key_id: KeyId,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
    pub time: TimingSettings,
    pub minimum_network_size: NonZeroU64,
    #[serde(skip)]
    pub recovery_data: RecoveryData,
    pub data_replication_factor: u64,
    pub broadcast: BroadcastSettings,
    pub blend_failure_fallback: bool,
}
