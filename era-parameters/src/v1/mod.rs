//! Version 1 of the parameter set: an era has a Blend, a blocks, a cryptarchia
//! and a time section, each in the version of its component the era runs.

use serde::{Deserialize, Serialize};

use crate::{
    Incompatible, Section, VersionGoesBack, blend::BlendParameters, blocks::BlocksParameters,
    cryptarchia::CryptarchiaParameters, time::TimeParameters,
};

pub(crate) mod codec;

/// Every section of an era's parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameters {
    pub blend: BlendParameters,
    pub blocks: BlocksParameters,
    pub cryptarchia: CryptarchiaParameters,
    pub time: TimeParameters,
}

impl Parameters {
    /// Checks that the versions of the sections can run together. Every
    /// combination is listed, so a new version of a section has to name the
    /// versions of the other sections it runs with: a service whose new
    /// version needs an operation only a later block layout carries runs only
    /// with that layout.
    ///
    /// # Errors
    ///
    /// If they cannot.
    pub const fn check_compatibility(&self) -> Result<(), Incompatible> {
        match (&self.blend, &self.blocks, &self.cryptarchia, &self.time) {
            (
                BlendParameters::V1(_),
                BlocksParameters::V1,
                CryptarchiaParameters::V1(_),
                TimeParameters::V1(_),
            ) => Ok(()),
        }
    }

    /// Checks that no section goes back to an older version than in
    /// `previous`.
    ///
    /// # Errors
    ///
    /// If one does.
    pub const fn check_follows(&self, previous: &Self) -> Result<(), VersionGoesBack> {
        let Self {
            blend,
            blocks,
            cryptarchia,
            time,
        } = self;
        let sections = [
            (Section::Blend, previous.blend.version(), blend.version()),
            (Section::Blocks, previous.blocks.version(), blocks.version()),
            (
                Section::Cryptarchia,
                previous.cryptarchia.version(),
                cryptarchia.version(),
            ),
            (Section::Time, previous.time.version(), time.version()),
        ];
        let mut index = 0;
        while index < sections.len() {
            let (section, previous, next) = sections[index];
            if next < previous {
                return Err(VersionGoesBack {
                    section,
                    previous,
                    next,
                });
            }
            index += 1;
        }
        Ok(())
    }
}
