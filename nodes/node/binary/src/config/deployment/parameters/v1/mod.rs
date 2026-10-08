//! Version 1 of the parameter set: an era has a Blend, a cryptarchia and a
//! time section, each in the version of its component the era runs.

use serde::{Deserialize, Serialize};

use crate::config::deployment::parameters::{
    VersionGoesBack, blend::BlendParameters, cryptarchia::CryptarchiaParameters,
    time::TimeParameters,
};

pub(crate) mod codec;

/// Every section of an era's parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub blend: BlendParameters,
    pub cryptarchia: CryptarchiaParameters,
    pub time: TimeParameters,
}

impl Parameters {
    /// Checks that no section goes back to an older version than in
    /// `previous`.
    ///
    /// # Errors
    ///
    /// If one does.
    pub const fn check_follows(&self, previous: &Self) -> Result<(), VersionGoesBack> {
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;
        if let Err(error) =
            VersionGoesBack::check("blend", previous.blend.version(), blend.version())
        {
            return Err(error);
        }
        if let Err(error) = VersionGoesBack::check(
            "cryptarchia",
            previous.cryptarchia.version(),
            cryptarchia.version(),
        ) {
            return Err(error);
        }
        VersionGoesBack::check("time", previous.time.version(), time.version())
    }
}
