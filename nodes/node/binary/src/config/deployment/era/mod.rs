use core::{iter, num::NonZero};
use std::collections::BTreeMap;

use ::serde::{Deserialize, Deserializer, Serialize};
use lb_cryptarchia_engine::{Epoch, era::MAX_ERAS_AFTER_GENESIS};
use lb_era_parameters::EraParameters;

mod serde;
use serde::EraScheduleVisitor;
#[cfg(test)]
mod tests;

pub(super) const GENESIS_EPOCH: Epoch = Epoch::new(0);

/// The eras of a chain, each keyed by the epoch it starts at. An era's
/// parameters are in force from that epoch until the next era starts.
///
/// A schedule is (de)serialized as a map from first epochs to era parameters,
/// and built from a `BTreeMap`, which keeps first epochs unique and ordered.
/// The first era must start at genesis. Deserialization also requires the eras
/// to be listed by strictly increasing first epoch, which rules out a repeated
/// epoch too.
#[derive(Serialize, Debug, Clone)]
#[serde(into = "BTreeMap<Epoch, EraParameters>")]
pub struct EraSchedule {
    /// The era that starts at genesis, which every schedule has.
    genesis: EraParameters,
    /// The eras after it, keyed by the epoch each starts at.
    after_genesis: BTreeMap<NonZero<u32>, EraParameters>,
}

impl EraSchedule {
    /// A schedule made of a single era, starting at genesis.
    #[must_use]
    pub const fn new_genesis(parameters: EraParameters) -> Self {
        Self {
            genesis: parameters,
            after_genesis: BTreeMap::new(),
        }
    }

    /// The parameters of the era that starts at genesis.
    #[must_use]
    pub const fn genesis(&self) -> &EraParameters {
        &self.genesis
    }

    pub const fn genesis_mut(&mut self) -> &mut EraParameters {
        &mut self.genesis
    }

    #[must_use]
    pub fn into_genesis(self) -> EraParameters {
        self.genesis
    }

    /// The parameters of the eras after the genesis era, keyed by the epoch
    /// each starts at.
    #[must_use]
    pub const fn after_genesis(&self) -> &BTreeMap<NonZero<u32>, EraParameters> {
        &self.after_genesis
    }

    /// Every era of the schedule with the epoch it starts at, in activation
    /// order.
    #[cfg(test)]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Epoch, &EraParameters)> {
        iter::once((GENESIS_EPOCH, &self.genesis))
            .chain(
                self.after_genesis
                    .iter()
                    .map(|(first_epoch, parameters)| (Epoch::new(first_epoch.get()), parameters)),
            )
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl From<EraSchedule> for BTreeMap<Epoch, EraParameters> {
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
                    .map(|(first_epoch, parameters)| (Epoch::new(first_epoch.get()), parameters)),
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
}

impl TryFrom<BTreeMap<Epoch, EraParameters>> for EraSchedule {
    type Error = EraScheduleError;

    /// Builds a schedule from eras keyed by their first epoch. The map keeps
    /// them unique and ordered, so what remains to check is that the first one
    /// starts at genesis, and that there are no more after it than a chain can
    /// number.
    fn try_from(mut eras: BTreeMap<Epoch, EraParameters>) -> Result<Self, Self::Error> {
        let Some((first_epoch, genesis)) = eras.pop_first() else {
            return Err(EraScheduleError::Empty);
        };
        if first_epoch != GENESIS_EPOCH {
            return Err(EraScheduleError::FirstEraAfterGenesis(first_epoch));
        }
        if eras.len() > MAX_ERAS_AFTER_GENESIS {
            return Err(EraScheduleError::TooManyEras(eras.len()));
        }
        let after_genesis = eras
            .into_iter()
            .map(|(first_epoch, parameters)| {
                let first_epoch = NonZero::new(first_epoch.into_inner())
                    .expect("the eras after the first, which starts at epoch 0, start later");
                (first_epoch, parameters)
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
