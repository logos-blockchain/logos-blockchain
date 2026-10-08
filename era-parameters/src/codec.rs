use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::{EraParameters, v1};

/// The version of the parameter set, then its layout in that version.
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
    fixture_parameters() => &format!("0100{}", v1::codec::parameters_hex())
);
