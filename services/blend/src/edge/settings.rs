use core::num::NonZeroU64;
use std::sync::Arc;

use lb_key_management_system_service::{backend::preload::KeyId, keys::UnsecuredEd25519Key};
use lb_poq::Quota;
use rayon::ThreadPool;

use crate::{core::settings::CoverTrafficSettings, settings::TimingSettings};

#[derive(Clone, Debug)]
pub struct StartingBlendConfig<BackendSettings, BroadcastSettings> {
    pub backend: BackendSettings,
    pub time: TimingSettings,
    pub non_ephemeral_signing_key_id: KeyId,
    pub num_blend_layers: NonZeroU64,
    pub minimum_network_size: NonZeroU64,
    pub cover: CoverTrafficSettings,
    /// `R_c`: replication factor for data messages.
    pub data_replication_factor: u64,
    pub max_blend_delay_in_rounds: NonZeroU64,
    pub broadcast: BroadcastSettings,
    pub blend_failure_fallback: bool,
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
    pub pow_mining_pool: Arc<ThreadPool>,
    pub max_blend_delay_in_rounds: NonZeroU64,
    pub blend_failure_fallback: bool,
}

impl<BackendSettings> RunningBlendConfig<BackendSettings> {
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
        match NonZeroU64::new(
            self.num_blend_layers
                .get()
                .saturating_mul(self.max_blend_delay_in_rounds.get().saturating_add(1)),
        ) {
            Some(deadline) => deadline,
            // Not `expect`, to keep this a `const fn`.
            None => panic!("Both factors of the delivery deadline are non-zero."),
        }
    }
}
