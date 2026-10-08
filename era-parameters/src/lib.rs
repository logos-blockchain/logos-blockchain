//! The parameters of an era, a section per component, and what a node derives
//! from them: the digests and the protocol names of the era.
//!
//! Each section is in the version of its component the era runs, and versions
//! change section by section: an era after genesis declares only the sections
//! it changes ([`EraChanges`]), so a deployment moves a component to a new
//! version at an era of its own choosing, leaving the others as they are.

use core::{fmt, num::NonZero, time::Duration};

use lb_binary_codec::canonical::BinaryEncode;
use lb_core::era::{EraDigest, ForkDigest};
use lb_cryptarchia_engine::era::BlockVersion;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    blend::BlendParameters, blocks::BlocksParameters, cryptarchia::CryptarchiaParameters,
    time::TimeParameters,
};

pub mod blend;
pub mod blocks;
pub mod cryptarchia;
pub mod time;

mod codec;
mod protocols;
pub use protocols::ProtocolNames;

/// The parameters an era runs under: a section per component, each in the
/// version of the component the era runs. Everything every node on the chain
/// must agree on while the era is in force.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EraParameters {
    pub blend: BlendParameters,
    pub blocks: BlocksParameters,
    pub cryptarchia: CryptarchiaParameters,
    pub time: TimeParameters,
}

impl EraParameters {
    /// The layout of the era's blocks.
    #[must_use]
    pub const fn block_version(&self) -> BlockVersion {
        match self.blocks {
            BlocksParameters::V1 => BlockVersion::V1,
        }
    }

    /// How long each slot of the era lasts.
    #[must_use]
    pub const fn slot_duration(&self) -> Duration {
        match &self.time {
            TimeParameters::V1(time) => time.slot_duration,
        }
    }

    /// The number of slots in each epoch of the era.
    #[must_use]
    pub const fn epoch_length(&self) -> NonZero<u64> {
        match &self.cryptarchia {
            CryptarchiaParameters::V1(cryptarchia) => NonZero::new(cryptarchia.slots_per_epoch())
                .expect("an epoch has at least one slot: its phases and base period are not zero"),
        }
    }

    /// How many slots, from the era's first, the network keeps accepting the
    /// identifiers of the era before it.
    #[must_use]
    pub const fn transition_slots(&self) -> u64 {
        match &self.blend {
            BlendParameters::V1(blend) => blend.transition_slots(),
        }
    }

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

    /// The parameters of the era that makes `changes` to an era of these
    /// parameters.
    ///
    /// # Errors
    ///
    /// If the era changes nothing, restates a section as it is, moves a
    /// section to an older version, or ends up with sections that cannot run
    /// together. An era's digest is over the sections it declares, so a
    /// schedule is written one way only; and a component's version only goes
    /// forward, so state only ever crosses into a version from an older one.
    pub fn with_changes(&self, changes: &EraChanges) -> Result<Self, ChangeError> {
        let EraChanges {
            blend,
            blocks,
            cryptarchia,
            time,
        } = changes;
        if blend.is_none() && blocks.is_none() && cryptarchia.is_none() && time.is_none() {
            return Err(ChangeError::NoChange);
        }
        let changed = Self {
            blend: changed_section(&self.blend, blend.as_ref())?,
            blocks: changed_section(&self.blocks, blocks.as_ref())?,
            cryptarchia: changed_section(&self.cryptarchia, cryptarchia.as_ref())?,
            time: changed_section(&self.time, time.as_ref())?,
        };
        changed.check_compatibility()?;
        Ok(changed)
    }
}

/// The sections an era declares, which its digest is over.
///
/// The genesis era declares every section, and an era after it the ones it
/// changes: a section an era leaves out stays as the era before it had it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EraChanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<BlocksParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cryptarchia: Option<CryptarchiaParameters>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeParameters>,
}

/// What the genesis era declares: every section.
impl From<EraParameters> for EraChanges {
    fn from(
        EraParameters {
            blend,
            blocks,
            cryptarchia,
            time,
        }: EraParameters,
    ) -> Self {
        Self {
            blend: Some(blend),
            blocks: Some(blocks),
            cryptarchia: Some(cryptarchia),
            time: Some(time),
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

impl Section {
    /// The section's identifier in the canonical encoding, fixed for good: a
    /// section added later takes the next one.
    const fn id(self) -> u8 {
        match self {
            Self::Blend => 0,
            Self::Blocks => 1,
            Self::Cryptarchia => 2,
            Self::Time => 3,
        }
    }
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

/// The parameters of one section, in one of its component's versions.
trait SectionParameters: BinaryEncode + Clone {
    const SECTION: Section;

    /// The section's version, as written ahead of its layout.
    fn version(&self) -> u16;
}

/// The section `in_force` becomes in an era that declares `declared`.
fn changed_section<Parameters>(
    in_force: &Parameters,
    declared: Option<&Parameters>,
) -> Result<Parameters, ChangeError>
where
    Parameters: SectionParameters,
{
    let Some(declared) = declared else {
        return Ok(in_force.clone());
    };
    let (previous, next) = (in_force.version(), declared.version());
    if next < previous {
        return Err(ChangeError::VersionGoesBack {
            section: Parameters::SECTION,
            previous,
            next,
        });
    }
    if declared.encode() == in_force.encode() {
        return Err(ChangeError::Unchanged(Parameters::SECTION));
    }
    Ok(declared.clone())
}

/// Why an era's changes cannot follow the era before it.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChangeError {
    #[error("the era changes no section")]
    NoChange,
    #[error("the era restates the {0} section as it is")]
    Unchanged(Section),
    #[error(
        "the era's {section} section is of version {next}, older than the version {previous} before it"
    )]
    VersionGoesBack {
        section: Section,
        previous: u16,
        next: u16,
    },
    #[error(transparent)]
    Incompatible(#[from] Incompatible),
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

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use lb_binary_codec::canonical::BinaryEncode as _;

    use crate::{
        ChangeError, EraChanges, EraParameters, Section, blend::BlendParameters,
        blocks::BlocksParameters, cryptarchia::CryptarchiaParameters, time::TimeParameters,
    };

    fn parameters() -> EraParameters {
        EraParameters {
            blend: BlendParameters::V1(crate::blend::v1::codec::fixture_settings()),
            blocks: BlocksParameters::V1,
            cryptarchia: CryptarchiaParameters::V1(
                crate::cryptarchia::v1::codec::fixture_settings(),
            ),
            time: TimeParameters::V1(crate::time::v1::codec::fixture_settings()),
        }
    }

    /// An era that changes the time section only, to slots of `slot_duration`.
    const fn slot_duration_change(slot_duration: Duration) -> EraChanges {
        EraChanges {
            blend: None,
            blocks: None,
            cryptarchia: None,
            time: Some(TimeParameters::V1(crate::time::v1::Settings {
                slot_duration,
            })),
        }
    }

    #[test]
    fn an_era_takes_the_sections_it_declares_and_keeps_the_others() {
        let parameters = parameters();
        let changed = parameters
            .with_changes(&slot_duration_change(Duration::from_secs(7)))
            .unwrap();

        assert_eq!(changed.slot_duration(), Duration::from_secs(7));
        assert_eq!(changed.blend.encode(), parameters.blend.encode());
        assert_eq!(changed.blocks.encode(), parameters.blocks.encode());
        assert_eq!(
            changed.cryptarchia.encode(),
            parameters.cryptarchia.encode()
        );
    }

    #[test]
    fn an_era_that_changes_nothing_is_refused() {
        let no_change = EraChanges {
            blend: None,
            blocks: None,
            cryptarchia: None,
            time: None,
        };

        assert_eq!(
            parameters().with_changes(&no_change).unwrap_err(),
            ChangeError::NoChange
        );
    }

    #[test]
    fn an_era_that_restates_a_section_as_it_is_is_refused() {
        let parameters = parameters();
        let restated = slot_duration_change(parameters.slot_duration());

        assert_eq!(
            parameters.with_changes(&restated).unwrap_err(),
            ChangeError::Unchanged(Section::Time)
        );
    }
}
