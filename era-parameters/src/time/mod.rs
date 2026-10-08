//! The time section of an era's parameters.

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use serde::{Deserialize, Serialize};

use crate::{Section, SectionParameters};

pub mod v1;

/// The time section, in the version of the time parameters the era runs.
///
/// Serialized as serde's externally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimeParameters {
    V1(v1::Settings),
}

impl SectionParameters for TimeParameters {
    const SECTION: Section = Section::Time;

    fn version(&self) -> u16 {
        match self {
            Self::V1(_) => 1,
        }
    }
}

/// The section's version, then its layout in that version.
impl BinaryEncode for TimeParameters {
    fn encoded_length(&self) -> usize {
        self.version().encoded_length()
            + match self {
                Self::V1(settings) => settings.encoded_length(),
            }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.version().encode_into(out);
        match self {
            Self::V1(settings) => settings.encode_into(out),
        }
    }
}

codec_fixtures!(
    TimeParameters,
    encode_only,
    Self::V1(v1::codec::fixture_settings()) => &format!("0100{}", v1::codec::SETTINGS_HEX)
);
