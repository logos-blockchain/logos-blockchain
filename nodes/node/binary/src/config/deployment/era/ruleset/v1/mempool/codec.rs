//! The fixtures of the canonical encoding of the mempool era parameters.

use core::num::NonZero;

use lb_binary_codec::canonical::codec_fixtures;

use super::Settings;

pub const fn fixture_settings() -> Settings {
    Settings {
        transaction_ttl_in_seconds: NonZero::new(43).unwrap(),
        retirement_grace_in_seconds: NonZero::new(44).unwrap(),
    }
}

pub const SETTINGS_HEX: &str = "2b00000000000000 2c00000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
