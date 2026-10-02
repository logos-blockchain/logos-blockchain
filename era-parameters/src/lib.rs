//! The parameters of an era, in the layout of each version, and what a node
//! derives from them: the digests and the protocol names of the era.

use core::{num::NonZero, time::Duration};

use lb_core::era::{EraDigest, ForkDigest};
use lb_cryptarchia_engine::era::EraVersion;
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
    /// The version of the parameter set.
    #[must_use]
    pub const fn version(&self) -> EraVersion {
        match self {
            Self::V1(_) => EraVersion::V1,
        }
    }

    const fn tag(&self) -> u16 {
        self.version().tag()
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

    /// The ledger's configuration while the era is in force.
    #[must_use]
    pub fn ledger_config(&self) -> lb_ledger::Config {
        match self {
            Self::V1(parameters) => parameters.ledger_config(),
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
