use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::{EraParameters, v1};

/// The tag of the parameters' version, then the layout of that version.
impl BinaryEncode for EraParameters {
    fn encoded_length(&self) -> usize {
        self.tag().encoded_length()
            + match self {
                Self::V1(parameters) => parameters.encoded_length(),
            }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.tag().encode_into(out);
        match self {
            Self::V1(parameters) => parameters.encode_into(out),
        }
    }
}

fn fixture_parameters() -> EraParameters {
    EraParameters::V1(v1::codec::fixture_parameters())
}

codec_fixtures!(
    EraParameters,
    encode_only,
    // Version 1's bytes are the ones every era encoded before parameters had
    // versions, so the digests of eras already scheduled do not move.
    fixture_parameters() => &format!("0100{}", v1::codec::settings_hex())
);
