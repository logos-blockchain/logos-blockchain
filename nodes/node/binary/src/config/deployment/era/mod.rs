use core::{iter, num::NonZero};
use std::collections::BTreeMap;

use ::serde::{Deserialize, Deserializer, Serialize};
use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_cryptarchia_engine::{
    Epoch,
    era::{BlockVersion, MAX_ERAS_AFTER_GENESIS},
};

use crate::config::deployment::parameters::{
    EraParameters, VersionGoesBack, blend::BlendParameters, cryptarchia::CryptarchiaParameters,
    time::TimeParameters, v1,
};

mod serde;
use serde::EraScheduleVisitor;
#[cfg(test)]
mod tests;

pub(super) const GENESIS_EPOCH: Epoch = Epoch::new(0);

/// An era as a deployment file declares it: the version of its blocks, and its
/// parameters. Its digest is over both.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EraDeclaration {
    pub block_version: BlockVersion,
    pub parameters: EraParameters,
}

impl EraDeclaration {
    /// Checks that the era's block version and the versions of its sections
    /// can run together. Every combination is listed, so a new block version
    /// or a new version of a section has to name the versions it runs with: a
    /// service whose new version needs an operation only a later block layout
    /// carries runs only with that layout.
    ///
    /// # Errors
    ///
    /// If they cannot.
    pub const fn check_compatibility(&self) -> Result<(), Incompatible> {
        match (self.block_version, &self.parameters) {
            (
                BlockVersion::V1,
                EraParameters::V1(v1::Parameters {
                    blend: BlendParameters::V1(_),
                    cryptarchia: CryptarchiaParameters::V1(_),
                    time: TimeParameters::V1(_),
                }),
            ) => Ok(()),
        }
    }

    /// Checks that this era can follow `previous`: neither its block version
    /// nor any of its sections goes back to an older version, so the state of
    /// a chain only ever crosses into a version from an older one.
    ///
    /// # Errors
    ///
    /// If one goes back.
    pub const fn check_follows(&self, previous: &Self) -> Result<(), VersionGoesBack> {
        if let Err(error) = VersionGoesBack::check(
            "block",
            block_version_tag(previous.block_version),
            block_version_tag(self.block_version),
        ) {
            return Err(error);
        }
        self.parameters.check_follows(&previous.parameters)
    }
}

/// The block version as written ahead of the era's parameters.
const fn block_version_tag(block_version: BlockVersion) -> u16 {
    match block_version {
        BlockVersion::V1 => 1,
    }
}

/// The block version, then the parameters.
impl BinaryEncode for EraDeclaration {
    fn encoded_length(&self) -> usize {
        let Self {
            block_version,
            parameters,
        } = self;

        block_version_tag(*block_version).encoded_length() + parameters.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            block_version,
            parameters,
        } = self;

        block_version_tag(*block_version).encode_into(out);
        parameters.encode_into(out);
    }
}

fn fixture_declaration() -> EraDeclaration {
    EraDeclaration {
        block_version: BlockVersion::V1,
        parameters: EraParameters::V1(v1::codec::fixture_parameters()),
    }
}

codec_fixtures!(
    EraDeclaration,
    encode_only,
    fixture_declaration() => &format!("0100 0100{}", v1::codec::parameters_hex())
);

/// Why the block version and the sections of an era cannot run together: the
/// versions they are of.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "blocks of {block_version:?} cannot run with blend version {blend}, cryptarchia version {cryptarchia} and time version {time}"
)]
pub struct Incompatible {
    pub block_version: BlockVersion,
    pub blend: u16,
    pub cryptarchia: u16,
    pub time: u16,
}

/// The eras of a chain, each keyed by the epoch it starts at. An era is in
/// force from that epoch until the next era starts.
///
/// A schedule is (de)serialized as a map from first epochs to era
/// declarations, and built from a `BTreeMap`, which keeps first epochs unique
/// and ordered. The first era must start at genesis. Deserialization also
/// requires the eras to be listed by strictly increasing first epoch, which
/// rules out a repeated epoch too.
#[derive(Serialize, Debug, Clone)]
#[serde(into = "BTreeMap<Epoch, EraDeclaration>")]
pub struct EraSchedule {
    /// The era that starts at genesis, which every schedule has.
    genesis: EraDeclaration,
    /// The eras after it, keyed by the epoch each starts at.
    after_genesis: BTreeMap<NonZero<u32>, EraDeclaration>,
}

impl EraSchedule {
    /// A schedule made of a single era, starting at genesis.
    #[must_use]
    pub const fn new_genesis(declaration: EraDeclaration) -> Self {
        Self {
            genesis: declaration,
            after_genesis: BTreeMap::new(),
        }
    }

    /// The era that starts at genesis.
    #[must_use]
    pub const fn genesis(&self) -> &EraDeclaration {
        &self.genesis
    }

    pub const fn genesis_mut(&mut self) -> &mut EraDeclaration {
        &mut self.genesis
    }

    #[must_use]
    pub fn into_genesis(self) -> EraDeclaration {
        self.genesis
    }

    /// The eras after the genesis era, keyed by the epoch each starts at.
    #[must_use]
    pub const fn after_genesis(&self) -> &BTreeMap<NonZero<u32>, EraDeclaration> {
        &self.after_genesis
    }

    /// Every era of the schedule with the epoch it starts at, in activation
    /// order.
    #[cfg(test)]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Epoch, &EraDeclaration)> {
        iter::once((GENESIS_EPOCH, &self.genesis))
            .chain(
                self.after_genesis
                    .iter()
                    .map(|(first_epoch, era)| (Epoch::new(first_epoch.get()), era)),
            )
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl From<EraSchedule> for BTreeMap<Epoch, EraDeclaration> {
    fn from(
        EraSchedule {
            genesis,
            after_genesis,
        }: EraSchedule,
    ) -> Self {
        iter::once((GENESIS_EPOCH, genesis))
            .chain(
                after_genesis
                    .into_iter()
                    .map(|(first_epoch, era)| (Epoch::new(first_epoch.get()), era)),
            )
            .collect()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EraScheduleError {
    #[error("the era schedule must contain at least one era")]
    Empty,
    #[error(
        "the first era must start at epoch {}, not at epoch {}",
        GENESIS_EPOCH.into_inner(),
        .0.into_inner()
    )]
    FirstEraAfterGenesis(Epoch),
    #[error(
        "eras must be listed by strictly increasing first epoch, but epoch {} follows epoch {}",
        .next.into_inner(),
        .previous.into_inner()
    )]
    OutOfOrder { previous: Epoch, next: Epoch },
    #[error("a schedule has at most {MAX_ERAS_AFTER_GENESIS} eras after its genesis era, not {0}")]
    TooManyEras(usize),
    #[error("the era from epoch {} cannot run: {source}", .epoch.into_inner())]
    Incompatible { epoch: Epoch, source: Incompatible },
    #[error("the era from epoch {} cannot follow the era before it: {source}", .epoch.into_inner())]
    VersionGoesBack {
        epoch: Epoch,
        source: VersionGoesBack,
    },
}

impl TryFrom<BTreeMap<Epoch, EraDeclaration>> for EraSchedule {
    type Error = EraScheduleError;

    /// Builds a schedule from eras keyed by their first epoch. The map keeps
    /// them unique and ordered, so what remains to check is that the first one
    /// starts at genesis, that there are no more after it than a chain can
    /// number, that every era's block version and sections can run together,
    /// and that no era goes back to an older version of anything.
    fn try_from(mut eras: BTreeMap<Epoch, EraDeclaration>) -> Result<Self, Self::Error> {
        let Some((first_epoch, genesis)) = eras.pop_first() else {
            return Err(EraScheduleError::Empty);
        };
        if first_epoch != GENESIS_EPOCH {
            return Err(EraScheduleError::FirstEraAfterGenesis(first_epoch));
        }
        if eras.len() > MAX_ERAS_AFTER_GENESIS {
            return Err(EraScheduleError::TooManyEras(eras.len()));
        }
        genesis
            .check_compatibility()
            .map_err(|source| EraScheduleError::Incompatible {
                epoch: GENESIS_EPOCH,
                source,
            })?;
        let mut previous = &genesis;
        for (&epoch, era) in &eras {
            era.check_compatibility()
                .map_err(|source| EraScheduleError::Incompatible { epoch, source })?;
            era.check_follows(previous)
                .map_err(|source| EraScheduleError::VersionGoesBack { epoch, source })?;
            previous = era;
        }
        let after_genesis = eras
            .into_iter()
            .map(|(first_epoch, era)| {
                let first_epoch = NonZero::new(first_epoch.into_inner())
                    .expect("the eras after the first, which starts at epoch 0, start later");
                (first_epoch, era)
            })
            .collect();
        Ok(Self {
            genesis,
            after_genesis,
        })
    }
}

impl<'de> Deserialize<'de> for EraSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(EraScheduleVisitor)
    }
}
