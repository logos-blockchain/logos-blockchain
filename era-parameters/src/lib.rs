//! The parameters of an era, a section per component, and what a node derives
//! from them: the digests and the protocol names of the era.
//!
//! Each section is in the version of its component the era runs, so a
//! deployment moves a component to a new version at an era of its own
//! choosing, leaving the others as they are. Every era states every section.

use core::{fmt, num::NonZero, time::Duration};

use lb_core::era::{EraDigest, ForkDigest};
use lb_cryptarchia_engine::era::BlockVersion;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod blend;
pub mod blocks;
pub mod cryptarchia;
pub mod time;
pub mod v1;

mod codec;
mod protocols;
pub use protocols::ProtocolNames;

/// The parameters an era runs under, in the version of the parameter set: the
/// sections it has. Each section is in the version of its component the era
/// runs, so the set's version changes only when a component is added or
/// removed. Everything every node on the chain must agree on while the era is
/// in force.
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

    /// The layout of the era's blocks.
    #[must_use]
    pub const fn block_version(&self) -> BlockVersion {
        match self {
            Self::V1(parameters) => parameters.blocks.block_version(),
        }
    }

    /// How long each slot of the era lasts.
    #[must_use]
    pub const fn slot_duration(&self) -> Duration {
        match self {
            Self::V1(parameters) => parameters.time.slot_duration(),
        }
    }

    /// The number of slots in each epoch of the era.
    #[must_use]
    pub const fn epoch_length(&self) -> NonZero<u64> {
        match self {
            Self::V1(parameters) => parameters.cryptarchia.epoch_length(),
        }
    }

    /// Checks that the versions of the sections can run together.
    ///
    /// # Errors
    ///
    /// If they cannot.
    pub const fn check_compatibility(&self) -> Result<(), Incompatible> {
        match self {
            Self::V1(parameters) => parameters.check_compatibility(),
        }
    }

    /// Checks that an era of these parameters can follow an era of `previous`:
    /// no section goes back to an older version, so the state of a chain only
    /// ever crosses into a version from an older one.
    ///
    /// # Errors
    ///
    /// If a section goes back.
    pub const fn check_follows(&self, previous: &Self) -> Result<(), VersionGoesBack> {
        match (previous, self) {
            (Self::V1(previous), Self::V1(next)) => next.check_follows(previous),
        }
    }
}

/// A section of an era's parameters, named after its component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Blend,
    Blocks,
    Cryptarchia,
    Time,
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Blend => "blend",
            Self::Blocks => "blocks",
            Self::Cryptarchia => "cryptarchia",
            Self::Time => "time",
        })
    }
}

/// Why an era cannot follow the era before it: a section of an older version
/// than before.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("the {section} section is of version {next}, older than the version {previous} before it")]
pub struct VersionGoesBack {
    pub section: Section,
    pub previous: u16,
    pub next: u16,
}

/// Why the sections of an era cannot run together: the versions they are of.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "blend version {blend}, blocks version {blocks}, cryptarchia version {cryptarchia} and time version {time} cannot run together"
)]
pub struct Incompatible {
    pub blend: u16,
    pub blocks: u16,
    pub cryptarchia: u16,
    pub time: u16,
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
