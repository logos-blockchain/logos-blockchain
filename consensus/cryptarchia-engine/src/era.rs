//! The eras of a chain.
//!
//! An era is a range of consecutive epochs governed by one set of parameters.
//! Eras are numbered from 0, the era that starts at genesis, and each one names
//! the version of its parameter set: the rules, the parameter layout and the
//! codecs it runs. Every era has its own slot duration and epoch length.

use core::{num::NonZero, time::Duration};

use lb_utils::bounded_duration::{MinimalBoundedDuration, SECOND};
use strum::{EnumIter, IntoEnumIterator as _};
use thiserror::Error;

use crate::time::Epoch;

/// An era, by its number: its position in its chain's era schedule, counting
/// from 0 for the era that starts at genesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Era(u16);

impl Era {
    /// Era 0, the era that starts at genesis.
    pub const GENESIS: Self = Self(0);

    #[must_use]
    pub const fn new(inner: u16) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn into_inner(self) -> u16 {
        self.0
    }
}

/// The version of an era's parameter set: the rules, the parameter layout and
/// the codecs that go with them.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    EnumIter,
    serde::Serialize,
    serde::Deserialize,
)]
#[repr(u16)]
#[serde(try_from = "u16", into = "u16")]
pub enum EraVersion {
    V1 = 1,
}

impl EraVersion {
    #[must_use]
    pub const fn tag(&self) -> u16 {
        *self as u16
    }

    fn variants_iter() -> impl Iterator<Item = Self> {
        Self::iter()
    }
}

impl From<EraVersion> for u16 {
    fn from(version: EraVersion) -> Self {
        version.tag()
    }
}

/// A tag no version of this release carries.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown era version {0}")]
pub struct UnknownEraVersion(pub u16);

impl TryFrom<u16> for EraVersion {
    type Error = UnknownEraVersion;

    fn try_from(tag: u16) -> Result<Self, Self::Error> {
        Self::variants_iter()
            .find(|version| version.tag() == tag)
            .ok_or(UnknownEraVersion(tag))
    }
}

/// An era as a schedule lists it: the epoch it starts at, the version of its
/// parameters, the length of its slots and epochs, and what it carries.
#[serde_with::serde_as]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EraEntry<Parameters> {
    pub first_epoch: Epoch,
    pub version: EraVersion,
    #[serde_as(as = "MinimalBoundedDuration<1, SECOND>")]
    pub slot_duration: Duration,
    pub epoch_length: NonZero<u64>,
    pub parameters: Parameters,
}

/// An era of a schedule: the era as the schedule lists it, and its number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledEra<Parameters> {
    pub era: Era,
    pub entry: EraEntry<Parameters>,
}

/// Why a list of eras cannot be resolved.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ErasError {
    #[error("a chain needs at least one era")]
    Empty,
    #[error("the first era must start at epoch 0, not at epoch {}", .0.into_inner())]
    FirstEraAfterGenesis(Epoch),
    #[error(
        "eras must start at strictly increasing epochs, but epoch {} follows epoch {}",
        .next.into_inner(),
        .previous.into_inner()
    )]
    OutOfOrder { previous: Epoch, next: Epoch },
    #[error("a chain has at most {} eras", u32::from(u16::MAX) + 1)]
    TooManyEras,
}

/// A chain's eras, in schedule order.
///
/// Never empty, and the first era starts at genesis, at epoch 0; era `n` is
/// the `n`-th entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eras<Parameters> {
    genesis: ScheduledEra<Parameters>,
    after_genesis: Vec<ScheduledEra<Parameters>>,
}

impl<Parameters> Eras<Parameters> {
    /// Numbers `entries`, listed in schedule order.
    pub fn new(entries: impl IntoIterator<Item = EraEntry<Parameters>>) -> Result<Self, ErasError> {
        let mut entries = entries.into_iter();
        let genesis = entries.next().ok_or(ErasError::Empty)?;
        if genesis.first_epoch != Epoch::new(0) {
            return Err(ErasError::FirstEraAfterGenesis(genesis.first_epoch));
        }
        let genesis = ScheduledEra {
            era: Era::GENESIS,
            entry: genesis,
        };
        let mut after_genesis: Vec<ScheduledEra<Parameters>> = Vec::new();
        for entry in entries {
            let previous = after_genesis.last().unwrap_or(&genesis);
            let era = previous
                .era
                .into_inner()
                .checked_add(1)
                .map(Era::new)
                .ok_or(ErasError::TooManyEras)?;
            if entry.first_epoch <= previous.entry.first_epoch {
                return Err(ErasError::OutOfOrder {
                    previous: previous.entry.first_epoch,
                    next: entry.first_epoch,
                });
            }
            after_genesis.push(ScheduledEra { era, entry });
        }
        Ok(Self {
            genesis,
            after_genesis,
        })
    }

    /// Era 0, the era that starts at genesis.
    #[must_use]
    pub const fn genesis(&self) -> &ScheduledEra<Parameters> {
        &self.genesis
    }
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use super::{EraEntry, EraVersion, Eras, ErasError};
    use crate::time::Epoch;

    fn entry(first_epoch: u32) -> EraEntry<()> {
        EraEntry {
            first_epoch: Epoch::new(first_epoch),
            version: EraVersion::V1,
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).unwrap(),
            parameters: (),
        }
    }

    #[test]
    fn invalid_schedules_are_rejected() {
        assert_eq!(Eras::<()>::new([]), Err(ErasError::Empty));
        assert_eq!(
            Eras::new([entry(1)]),
            Err(ErasError::FirstEraAfterGenesis(Epoch::new(1)))
        );
        assert_eq!(
            Eras::new([entry(0), entry(5), entry(5)]),
            Err(ErasError::OutOfOrder {
                previous: Epoch::new(5),
                next: Epoch::new(5)
            })
        );
    }
}
