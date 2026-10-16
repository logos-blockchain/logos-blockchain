use core::{num::NonZeroU64, time::Duration};

use lb_cryptarchia_engine::EraSchedule;
use lb_key_management_system_service::backend::preload::KeyId;
use lb_poq::Quota;

use crate::{
    core::settings::EraCoverTrafficSettings,
    settings::{EraTimingSettings, max_data_message_delay_in_rounds},
};

/// The edge service's settings: this node's own, the same in every era, the
/// settings of its backend and its dispatcher, which the node composes, and its
/// own settings in every era, its backend's included.
#[derive(Clone, Debug)]
pub struct EdgeServiceSettings<BackendSettings, BackendEraSettings, DispatcherSettings> {
    /// The non-ephemeral signing key (NSK) corresponding to the public key
    /// registered in the membership (SDP).
    pub non_ephemeral_signing_key_id: KeyId,
    pub abstain_on_failure: bool,
    pub backend: BackendSettings,
    pub dispatcher: DispatcherSettings,
    pub era_schedule: EraSchedule<EdgeServiceEraSettings<BackendEraSettings>>,
}

/// The edge service's settings in an era, its backend's included.
#[derive(Clone, Debug)]
pub struct EdgeServiceEraSettings<BackendEraSettings> {
    pub time: EraTimingSettings,
    /// `ß_c`: number of blending operations for each locally generated message.
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub cover: EraCoverTrafficSettings,
    /// `R_c`: replication factor for data messages.
    pub data_replication_factor: u64,
    pub max_blend_delay_in_rounds: NonZeroU64,
    pub backend: BackendEraSettings,
}

impl<BackendEraSettings> EdgeServiceEraSettings<BackendEraSettings> {
    pub fn epoch_pow_quota(&self) -> Quota {
        self.num_blend_layers
            .get()
            .try_into()
            .expect("PoW Quota must fit within the width the `PoQ` circuit allows.")
    }

    pub const fn epoch_leadership_quota(&self) -> Quota {
        let num_blend_layers = self.num_blend_layers.get();
        let additional_encapsulations = num_blend_layers
            .checked_mul(self.data_replication_factor)
            .expect("Overflow when computing total replication factor.");
        let quota = num_blend_layers
            .checked_add(additional_encapsulations)
            .expect("Overflow when computing leadership quota.");
        match Quota::try_new(quota) {
            Ok(quota) => quota,
            Err(_) => {
                panic!("Leadership Quota must fit within the width the `PoQ` circuit allows.")
            }
        }
    }

    #[must_use]
    pub const fn max_data_message_delay_in_rounds(&self) -> NonZeroU64 {
        max_data_message_delay_in_rounds(
            self.num_blend_layers,
            self.max_blend_delay_in_rounds,
            self.time.network_absorption_in_rounds,
        )
    }

    /// `T_M` as a duration: how long the network has to deliver a data
    /// message released under these settings.
    #[must_use]
    pub const fn max_data_message_delay(&self) -> Duration {
        self.time
            .duration_of(self.max_data_message_delay_in_rounds().get())
    }
}
