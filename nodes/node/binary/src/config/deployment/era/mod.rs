use core::{iter, num::NonZero};
use std::collections::BTreeMap;

use ::serde::{Deserialize, Deserializer, Serialize};
use lb_cryptarchia_engine::{
    Epoch,
    era::{ErasError, MAX_ERAS_AFTER_GENESIS},
};
use lb_era_parameters::{ChangeError, EraChanges, EraParameters, Incompatible};

mod serde;
use serde::EraScheduleVisitor;
#[cfg(test)]
mod tests;

pub(super) const GENESIS_EPOCH: Epoch = Epoch::new(0);

/// The eras of a chain, each keyed by the epoch it starts at. An era's
/// parameters are in force from that epoch until the next era starts.
///
/// The genesis era declares every section of its parameters, and each era
/// after it the sections it changes. A schedule is (de)serialized as a map
/// from first epochs to what each era declares. The first era must start at
/// genesis, and deserialization also requires the eras to be listed by
/// strictly increasing first epoch, which rules out a repeated epoch too.
#[derive(Serialize, Debug, Clone)]
#[serde(into = "BTreeMap<Epoch, EraChanges>")]
pub struct EraSchedule {
    /// The era that starts at genesis, which every schedule has.
    genesis: EraParameters,
    /// The eras after it, keyed by the epoch each starts at.
    after_genesis: BTreeMap<NonZero<u32>, EraChanges>,
}

impl EraSchedule {
    /// A schedule of the genesis era, then of the eras after it, keyed by
    /// the epoch each starts at.
    ///
    /// # Errors
    ///
    /// If there are more eras than a chain can number, if the sections of the
    /// genesis era cannot run together, or if an era's changes cannot follow
    /// the era before it.
    pub fn new(
        genesis: EraParameters,
        after_genesis: BTreeMap<NonZero<u32>, EraChanges>,
    ) -> Result<Self, EraScheduleError> {
        if after_genesis.len() > MAX_ERAS_AFTER_GENESIS {
            return Err(EraScheduleError::TooManyEras(after_genesis.len()));
        }
        let schedule = Self {
            genesis,
            after_genesis,
        };
        schedule.resolve()?;
        Ok(schedule)
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

    /// What the eras after the genesis era change, keyed by the epoch each
    /// starts at.
    #[must_use]
    pub const fn after_genesis(&self) -> &BTreeMap<NonZero<u32>, EraChanges> {
        &self.after_genesis
    }

    /// Every era of the schedule, in activation order: the epoch it starts at,
    /// what it declares and the parameters it runs under.
    ///
    /// # Errors
    ///
    /// If the sections of the genesis era cannot run together, or if an era's
    /// changes cannot follow the era before it.
    pub(super) fn resolve(
        &self,
    ) -> Result<Vec<(Epoch, EraChanges, EraParameters)>, EraScheduleError> {
        self.genesis.check_compatibility()?;
        let genesis = (
            GENESIS_EPOCH,
            EraChanges::from(self.genesis.clone()),
            self.genesis.clone(),
        );
        let mut eras = Vec::with_capacity(self.after_genesis.len() + 1);
        eras.push(genesis);
        for (first_epoch, changes) in &self.after_genesis {
            let epoch = Epoch::new(first_epoch.get());
            let (_, _, previous) = eras.last().expect("the genesis era is resolved");
            let parameters = previous
                .with_changes(changes)
                .map_err(|source| EraScheduleError::Changes { epoch, source })?;
            eras.push((epoch, changes.clone(), parameters));
        }
        Ok(eras)
    }
}

impl From<EraSchedule> for BTreeMap<Epoch, EraChanges> {
    fn from(
        EraSchedule {
            genesis,
            after_genesis,
        }: EraSchedule,
    ) -> Self {
        iter::once((GENESIS_EPOCH, EraChanges::from(genesis)))
            .chain(
                after_genesis
                    .into_iter()
                    .map(|(first_epoch, changes)| (Epoch::new(first_epoch.get()), changes)),
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
    #[error("the sections of the genesis era cannot run together: {0}")]
    Incompatible(#[from] Incompatible),
    #[error("the era from epoch {} cannot follow the era before it: {source}", .epoch.into_inner())]
    Changes { epoch: Epoch, source: ChangeError },
    #[error(transparent)]
    Eras(#[from] ErasError),
}

impl<'de> Deserialize<'de> for EraSchedule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(EraScheduleVisitor)
    }
}
