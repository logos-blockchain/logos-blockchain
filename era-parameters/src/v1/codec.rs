use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::v1::{Parameters, blend, cryptarchia, time};

/// Version 1's layout: the Blend, cryptarchia and time parameters, in that
/// order.
impl BinaryEncode for Parameters {
    fn encoded_length(&self) -> usize {
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;

        blend.encoded_length() + cryptarchia.encoded_length() + time.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;

        blend.encode_into(out);
        cryptarchia.encode_into(out);
        time.encode_into(out);
    }
}

pub fn fixture_parameters() -> Parameters {
    Parameters {
        blend: blend::codec::fixture_settings(),
        cryptarchia: cryptarchia::codec::fixture_settings(),
        time: time::codec::fixture_settings(),
    }
}

/// The encoding of [`fixture_parameters`]: each section's, in order. A
/// function, since `&str` constants cannot be concatenated at compile time.
pub fn settings_hex() -> String {
    [
        blend::codec::SETTINGS_HEX,
        cryptarchia::codec::SETTINGS_HEX,
        time::codec::SETTINGS_HEX,
    ]
    .concat()
}

codec_fixtures!(
    Parameters,
    encode_only,
    fixture_parameters() => &settings_hex()
);
