use core::num::NonZeroU64;

use lb_key_management_system_service::backend::preload::KeyId;
use lb_services_utils::overwatch::RecoveryData;
use nutype::nutype;
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
    pub minimum_network_size: MinimumNetworkSize,
    #[serde(skip)]
    pub recovery_data: RecoveryData,
    pub data_replication_factor: u64,
    pub broadcast: BroadcastSettings,
    pub abstain_on_failure: bool,
}

/// The number of core nodes below which the node does not use Blend and
/// broadcasts directly (`blend-protocol.md` §Fallback).
///
/// At least two: a network of one node would blend with itself. The bound
/// lives here, on the settings the service consumes, rather than on the node's
/// deployment wrapper, so that no caller can hand the service a smaller value.
#[nutype(
    validate(greater_or_equal = 2),
    derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)
)]
pub struct MinimumNetworkSize(u64);

impl From<MinimumNetworkSize> for NonZeroU64 {
    fn from(value: MinimumNetworkSize) -> Self {
        Self::new(value.into_inner()).expect("a minimum network size of at least 2 is non-zero")
    }
}
