//! The canonical encoding of the types the node binary defines, for the digests
//! that commit to them.
//!
//! An era's parameters are the only such type today. Their encoding is the
//! preimage of the era's digest, so every node on a chain must produce it byte
//! for byte: the version of their layout as a little-endian `u16`, then the
//! Blend, cryptarchia and time parameters, in that order, each writing its
//! fields in declaration order, depth first. Types from lower
//! crates encode themselves in their own crates: an integer little-endian at
//! its width, a non-zero or otherwise range-checked integer as the integer it
//! wraps, a float as its IEEE 754 bits, a ratio as its numerator then its
//! denominator, a duration as its whole seconds then the nanoseconds past
//! them, an optional value as a `0` byte, or a `1` byte and the value, and a
//! map as its length, then each key and value in ascending key order. So the
//! SDP service parameters list each service in ascending order of service type.
//!
//! These encodings are never decoded: the parameters are read from the
//! deployment settings and encoded only to be hashed. Changing one changes the
//! digest of every era, so each is pinned by a fixture. In every fixture, a
//! field holds its position among the parameters that follow the version, so
//! the fixtures show where each field lands and how wide it is.
//!
//! Every `encode_into` destructures its type without `..`, so a field added to
//! any of them fails to compile until it is given its place in the encoding.

mod blend;
mod cryptarchia;
mod time;

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::config::deployment::EraParameters;

// TODO: This will be migrated when we migrate `EraParameters` from being a
// struct to be an enum, and we will encode the enum variant as `u16`, using `1`
// for the `V1` variant.
/// The version of the era parameters' layout, written ahead of them.
///
/// The parameters take a single layout today. A release that changes them
/// encodes the eras that adopt the change under a new version, and keeps
/// encoding the eras of earlier versions exactly as before: the digest of an
/// era already activated, and the fork digest of every chain that activated
/// it, never move.
const ERA_PARAMETERS_VERSION: u16 = 1;

impl BinaryEncode for EraParameters {
    fn encoded_length(&self) -> usize {
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;

        ERA_PARAMETERS_VERSION.encoded_length()
            + blend.encoded_length()
            + cryptarchia.encoded_length()
            + time.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;

        ERA_PARAMETERS_VERSION.encode_into(out);
        blend.encode_into(out);
        cryptarchia.encode_into(out);
        time.encode_into(out);
    }
}

fn fixture_era_parameters() -> EraParameters {
    EraParameters {
        blend: blend::fixture_settings(),
        cryptarchia: cryptarchia::fixture_settings(),
        time: time::fixture_settings(),
    }
}

codec_fixtures!(
    EraParameters,
    encode_only,
    // TODO: Once migrated to an enum, verify that the `V1` variant will encode to exactly the same bytes as now.
    fixture_era_parameters() => &["0100", blend::SETTINGS_HEX, cryptarchia::SETTINGS_HEX, time::SETTINGS_HEX].concat()
);
