//! The blocks section of an era's parameters: the layout of the era's blocks.

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_cryptarchia_engine::era::BlockVersion;
use serde::{Deserialize, Serialize};

/// The blocks section, in the version of the block layout the era runs.
/// Version 1 has no parameters of its own.
///
/// Serialized as serde's externally tagged enum: a version without parameters
/// as its bare name, such as `V1`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BlocksParameters {
    V1,
}

impl BlocksParameters {
    /// The section's version, as written ahead of its layout.
    #[must_use]
    pub const fn version(&self) -> u16 {
        match self {
            Self::V1 => 1,
        }
    }

    /// The layout of the era's blocks.
    #[must_use]
    pub const fn block_version(self) -> BlockVersion {
        match self {
            Self::V1 => BlockVersion::V1,
        }
    }
}


/// The section's version, then its layout in that version, empty in version 1.
impl BinaryEncode for BlocksParameters {
    fn encoded_length(&self) -> usize {
        self.version().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.version().encode_into(out);
    }
}

codec_fixtures!(BlocksParameters, encode_only, Self::V1 => "0100");
