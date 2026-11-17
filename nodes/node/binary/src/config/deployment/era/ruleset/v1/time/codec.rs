//! The fixtures of the canonical encoding of the time era parameters.

use core::num::NonZero;

use lb_binary_codec::canonical::codec_fixtures;

use super::Settings;

pub const fn fixture_settings() -> Settings {
    Settings {
        slot_duration_in_seconds: NonZero::new(42).unwrap(),
    }
}

pub const SETTINGS_HEX: &str = "2a00000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
