use core::{num::NonZero, time::Duration};

use serde::{Deserialize, Serialize};

pub mod v1;

/// The rules an era runs under, named by their version, with the values of
/// their parameters in the layout that version defines.
///
/// An era that only changes values keeps its ruleset. A change of behaviour,
/// parameters or encoding needs a new one.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EraRuleset {
    V1(v1::Parameters),
}

impl EraRuleset {
    /// The ruleset's number, written ahead of its parameters in the era's
    /// digest. Only used for encoding the ruleset.
    #[must_use]
    pub(super) const fn version(&self) -> u16 {
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
