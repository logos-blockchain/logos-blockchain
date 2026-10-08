//! The Blend section of an era's parameters.

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use serde::{Deserialize, Serialize};

use crate::{Section, SectionParameters};

pub mod v1;

/// The Blend section, in the version of Blend the era runs.
///
/// Serialized as serde's externally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlendParameters {
    V1(v1::Settings),
}

impl SectionParameters for BlendParameters {
    const SECTION: Section = Section::Blend;

    fn version(&self) -> u16 {
        match self {
            Self::V1(_) => 1,
        }
    }
}

/// The section's version, then its layout in that version.
impl BinaryEncode for BlendParameters {
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
    BlendParameters,
    encode_only,
    Self::V1(v1::codec::fixture_settings()) => &format!("0100{}", v1::codec::SETTINGS_HEX)
);
