use core::num::NonZeroU64;

use lb_key_management_system_service::backend::preload::KeyId;

use crate::settings::TimingSettings;

/// The broadcast service's settings in an era, in Blend's own versions.
#[derive(Clone, Debug)]
pub enum EraSettings<NetworkSettings> {
    V1(StartingBlendConfig<NetworkSettings>),
}

/// What a broadcast node needs to run.
#[derive(Clone, Debug)]
pub struct StartingBlendConfig<NetworkSettings> {
    /// Where a payload goes: the same dispatcher settings a core node
    /// republishes through.
    pub network: NetworkSettings,
    pub time: TimingSettings,
    /// Only to derive this node's identity for the membership subscription.
    pub non_ephemeral_signing_key_id: KeyId,
    /// The threshold that decides whether broadcast is still the right mode.
    pub minimum_network_size: NonZeroU64,
}
