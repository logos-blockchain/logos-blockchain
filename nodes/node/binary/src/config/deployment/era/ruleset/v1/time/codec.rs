//! The canonical encoding of the time era parameters.

use core::num::NonZero;

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use super::Settings;

impl BinaryEncode for Settings {
    fn encoded_length(&self) -> usize {
        let Self {
            slot_duration_in_seconds,
        } = self;

        slot_duration_in_seconds.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            slot_duration_in_seconds,
        } = self;

        slot_duration_in_seconds.encode_into(out);
    }
}

pub const fn fixture_settings() -> Settings {
    Settings {
        slot_duration_in_seconds: NonZero::new(42).unwrap(),
    }
}

pub const SETTINGS_HEX: &str = "2a00000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
