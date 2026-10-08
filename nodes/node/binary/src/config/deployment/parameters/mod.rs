//! The parameters of an era, a section per component.
//!
//! Each section is in the version of its component the era runs, so a
//! deployment moves a component to a new version at an era of its own
//! choosing, leaving the others as they are. Every era states every section.

use core::{num::NonZero, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod blend;
pub mod cryptarchia;
pub mod time;
pub mod v1;

mod codec;

/// The parameters an era runs under, in the version of the parameter set: the
/// sections it has. Each section is in the version of its component the era
/// runs, so the set's version changes only when a component is added or
/// removed.
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

    /// Checks that no section goes back to an older version than in
    /// `previous`.
    ///
    /// # Errors
    ///
    /// If one does.
    pub const fn check_follows(&self, previous: &Self) -> Result<(), VersionGoesBack> {
        match (previous, self) {
            (Self::V1(previous), Self::V1(next)) => next.check_follows(previous),
        }
    }
}

/// Why an era cannot follow the era before it: something of an older version
/// than before.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("the {component} version is {next}, older than the version {previous} before it")]
pub struct VersionGoesBack {
    pub component: &'static str,
    pub previous: u16,
    pub next: u16,
}

impl VersionGoesBack {
    /// Checks that `component`'s version `next` is not older than `previous`.
    pub(crate) const fn check(
        component: &'static str,
        previous: u16,
        next: u16,
    ) -> Result<(), Self> {
        if next < previous {
            return Err(Self {
                component,
                previous,
                next,
            });
        }
        Ok(())
    }
}
