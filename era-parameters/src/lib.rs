//! The parameters of an era, in the layout of each version, and what a node
//! derives from them: the digests and the protocol names of the era.

use core::{num::NonZero, time::Duration};

use lb_core::era::{EraDigest, ForkDigest};
use serde::{Deserialize, Serialize};

pub mod v1;

mod codec;
mod protocols;
pub use protocols::ProtocolNames;

/// The parameters an era defines, in the layout of its version: everything
/// every node on the chain must agree on while the era is in force.
///
/// Serialized as serde's externally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EraParameters {
    V1(v1::Parameters),
}

impl EraParameters {
    /// The version of the parameter set, as written ahead of its encoding.
    #[must_use]
    pub const fn tag(&self) -> u16 {
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

    /// How many slots, from the era's first, the network keeps accepting the
    /// identifiers of the era before it.
    #[must_use]
    pub const fn transition_slots(&self) -> u64 {
        match self {
            Self::V1(parameters) => parameters.transition_slots(),
        }
    }
}

/// An era as the node runs it: its parameters, its digest, and the fork digest
/// and protocol names in force while it is.
#[derive(Clone, Debug)]
pub struct EraDefinition {
    pub parameters: EraParameters,
    pub digest: EraDigest,
    /// The digest of the eras up to this one, in activation order.
    pub fork_digest: ForkDigest,
    pub protocol_names: ProtocolNames,
}
