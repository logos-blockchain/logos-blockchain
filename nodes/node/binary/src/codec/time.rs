//! The canonical encoding of the time era parameters.

use core::time::Duration;

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};

use crate::config::time::deployment::Settings;

impl BinaryEncode for Settings {
    fn encoded_length(&self) -> usize {
        let Self { slot_duration } = self;

        slot_duration.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self { slot_duration } = self;

        slot_duration.encode_into(out);
    }
}

pub(super) const fn fixture_settings() -> Settings {
    Settings {
        slot_duration: Duration::from_secs(42),
    }
}

pub(super) const SETTINGS_HEX: &str = "2a00000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
