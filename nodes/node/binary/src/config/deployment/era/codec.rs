//! The canonical encoding of the types the node binary defines, for the digests
//! that commit to them.
//!
//! An era's ruleset is the only such type today. Its encoding is the preimage
//! of the era's digest, so every node on a chain must produce it byte for
//! byte: the ruleset's number as a little-endian `u16`, then its Blend,
//! cryptarchia and time parameters, in that order, each writing
//! its fields in declaration order, depth first. Types from lower
//! crates encode themselves in their own crates: an integer little-endian at
//! its width, a non-zero or otherwise range-checked integer as the integer it
//! wraps, a float as its IEEE 754 bits, a ratio as its numerator then its
//! denominator, a duration as its whole seconds then the nanoseconds past
//! them, an optional value as a `0` byte, or a `1` byte and the value, and a
//! map as its length, then each key and value in ascending key order. So the
//! SDP service parameters list each service in ascending order of service type.
//!
//! These encodings are never decoded: the rulesets are read from the
//! deployment settings and encoded only to be hashed. Changing one changes the
//! digest of every era, so each is pinned by a fixture. In every fixture, a
//! field holds its position among the parameters that follow the number, so
//! the fixtures show where each field lands and how wide it is.
//!
//! Every `encode_into` destructures its type without `..`, so a field added to
//! any of them fails to compile until it is given its place in the encoding.

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::config::deployment::era::ruleset::{EraRuleset, v1};

/// The ruleset's number, then its parameters in the layout it defines.
impl BinaryEncode for EraRuleset {
    fn encoded_length(&self) -> usize {
        self.version().encoded_length()
            + match self {
                Self::V1(v1::Parameters {
                    blend,
                    cryptarchia,
                    time,
                }) => blend.encoded_length() + cryptarchia.encoded_length() + time.encoded_length(),
            }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.version().encode_into(out);
        match self {
            Self::V1(v1::Parameters {
                blend,
                cryptarchia,
                time,
            }) => {
                blend.encode_into(out);
                cryptarchia.encode_into(out);
                time.encode_into(out);
            }
        }
    }
}

fn fixture_era_ruleset() -> EraRuleset {
    EraRuleset::V1(v1::Parameters {
        blend: v1::blend::codec::fixture_settings(),
        cryptarchia: v1::cryptarchia::codec::fixture_settings(),
        time: v1::time::codec::fixture_settings(),
    })
}

/// The encoding of [`fixture_era_ruleset`]: the same bytes the parameters
/// encoded to before they had a ruleset number. A function, since `&str`
/// constants cannot be concatenated at compile time.
fn era_ruleset_hex() -> String {
    [
        "0100",
        v1::blend::codec::SETTINGS_HEX,
        v1::cryptarchia::codec::SETTINGS_HEX,
        v1::time::codec::SETTINGS_HEX,
    ]
    .concat()
}

codec_fixtures!(
    EraRuleset,
    encode_only,
    fixture_era_ruleset() => &era_ruleset_hex()
);
