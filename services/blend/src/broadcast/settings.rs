use core::{num::NonZeroU64, time::Duration};

use lb_cryptarchia_engine::EraSchedule;
use lb_key_management_system_service::backend::preload::KeyId;

/// What a broadcast node needs to run: this node's own key, the settings of
/// its dispatcher, which the node composes, and its own settings in every era.
#[derive(Clone, Debug)]
pub struct BroadcastServiceSettings<DispatcherSettings> {
    /// Only to derive this node's identity for the membership subscription.
    pub non_ephemeral_signing_key_id: KeyId,
    /// Where a payload goes: the same dispatcher settings a core node
    /// republishes through.
    pub dispatcher: DispatcherSettings,
    pub era_schedule: EraSchedule<BroadcastServiceEraSettings>,
}

/// What a broadcast node needs of an era.
#[derive(Clone, Debug)]
pub struct BroadcastServiceEraSettings {
    /// The threshold that decides whether broadcast is still the right mode.
    pub minimum_network_size: NonZeroU64,
    pub epoch_transition_period: Duration,
}
