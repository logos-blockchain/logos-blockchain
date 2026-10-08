use core::{num::NonZero, time::Duration};

use serde::{Deserialize, Serialize};

pub mod v1;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EraParameters {
    V1(v1::Parameters),
}

impl EraParameters {
    #[must_use]
    pub const fn version(&self) -> u16 {
        match self {
            Self::V1(_) => 1,
        }
    }

    /// How long each slot of the era lasts.
    #[must_use]
    pub const fn slot_duration(&self) -> Duration {
        match self {
            Self::V1(parameters) => parameters.time.slot_duration,
        }
    }

    /// The number of slots in each epoch of the era.
    #[must_use]
    pub const fn epoch_length(&self) -> NonZero<u64> {
        match self {
            Self::V1(parameters) => NonZero::new(parameters.cryptarchia.slots_per_epoch())
                .expect("an epoch has at least one slot: its phases and base period are not zero"),
        }
    }
}
