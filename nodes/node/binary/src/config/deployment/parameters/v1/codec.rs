use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::config::deployment::parameters::{
    blend::BlendParameters, cryptarchia::CryptarchiaParameters, time::TimeParameters,
    v1::Parameters,
};

/// Version 1's layout: the Blend, cryptarchia and time sections, in that
/// order, each its version, then its layout in that version.
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
        blend: BlendParameters::V1(
            crate::config::deployment::parameters::blend::v1::codec::fixture_settings(),
        ),
        cryptarchia: CryptarchiaParameters::V1(
            crate::config::deployment::parameters::cryptarchia::v1::codec::fixture_settings(),
        ),
        time: TimeParameters::V1(
            crate::config::deployment::parameters::time::v1::codec::fixture_settings(),
        ),
    }
}

/// The encoding of [`fixture_parameters`]: each section's version and
/// encoding, in order. A function, since `&str` constants cannot be
/// concatenated at compile time.
pub fn parameters_hex() -> String {
    [
        "0100",
        crate::config::deployment::parameters::blend::v1::codec::SETTINGS_HEX,
        "0100",
        crate::config::deployment::parameters::cryptarchia::v1::codec::SETTINGS_HEX,
        "0100",
        crate::config::deployment::parameters::time::v1::codec::SETTINGS_HEX,
    ]
    .concat()
}

codec_fixtures!(
    Parameters,
    encode_only,
    fixture_parameters() => &parameters_hex()
);
