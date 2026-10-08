//! The cryptarchia section of an era's parameters: consensus, epochs, SDP and
//! `PoW`.

use core::num::NonZero;

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use serde::{Deserialize, Serialize};

pub mod v1;

/// The cryptarchia section, in the version of cryptarchia the era runs.
///
/// Serialized as serde's externally tagged enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CryptarchiaParameters {
    V1(v1::Settings),
}

impl CryptarchiaParameters {
    /// The section's version, as written ahead of its layout.
    #[must_use]
    pub const fn version(&self) -> u16 {
        match self {
            Self::V1(_) => 1,
        }
    }

    /// The number of slots in each epoch of the era.
    #[must_use]
    pub const fn epoch_length(&self) -> NonZero<u64> {
        match self {
            Self::V1(cryptarchia) => NonZero::new(cryptarchia.slots_per_epoch())
                .expect("an epoch has at least one slot: its phases and base period are not zero"),
        }
    }
}


/// The section's version, then its layout in that version.
impl BinaryEncode for CryptarchiaParameters {
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
    CryptarchiaParameters,
    encode_only,
    Self::V1(v1::codec::fixture_settings()) => &format!("0100{}", v1::codec::SETTINGS_HEX)
);
