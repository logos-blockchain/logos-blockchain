//! The canonical encoding of the Blend era parameters.

use core::num::{NonZeroU32, NonZeroU64, NonZeroU128};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_utils::math::PositiveRatio;

use super::{
    CommonSettings, CoreSettings, CoverReplicationFactor, CoverTrafficSettings,
    MessageDelayerSettings, MinimumNetworkSize, SchedulerSettings, Settings,
};

// A minimum network size: the size it wraps. Hand-written because `#[nutype]`
// refuses a `#[derive]` on its type.
impl BinaryEncode for MinimumNetworkSize {
    fn encoded_length(&self) -> usize {
        self.into_inner().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.into_inner().encode_into(out);
    }
}

pub fn fixture_settings() -> Settings {
    Settings {
        common: fixture_common_settings(),
        core: fixture_core_settings(),
    }
}

fn fixture_common_settings() -> CommonSettings {
    CommonSettings {
        num_blend_layers: NonZeroU64::new(1).unwrap(),
        minimum_network_size: fixture_minimum_network_size(),
        network_absorption_in_rounds: NonZeroU64::new(3).unwrap(),
        data_replication_factor: 4,
    }
}

fn fixture_minimum_network_size() -> MinimumNetworkSize {
    MinimumNetworkSize::try_new(2).unwrap()
}

const fn fixture_core_settings() -> CoreSettings {
    CoreSettings {
        scheduler: scheduler_settings(),
        target_peering_degree: NonZeroU32::new(7).unwrap(),
        verification_rate_per_second: NonZeroU32::new(8).unwrap(),
        edge_node_send_deadline_in_rounds: NonZeroU64::new(9).unwrap(),
        core_handshake_deadline_in_rounds: NonZeroU128::new(10).unwrap(),
        expected_deliveries_per_observation_window: NonZeroU64::new(11).unwrap(),
        activity_threshold_sensitivity: 12,
    }
}

const fn scheduler_settings() -> SchedulerSettings {
    SchedulerSettings {
        cover: cover_traffic_settings(),
        delayer: message_delayer_settings(),
    }
}

const fn cover_traffic_settings() -> CoverTrafficSettings {
    CoverTrafficSettings {
        message_frequency_per_round: PositiveRatio::new(5, NonZeroU32::new(13).unwrap()).unwrap(),
        replication_factor: fixture_cover_replication_factor(),
    }
}

const fn fixture_cover_replication_factor() -> CoverReplicationFactor {
    CoverReplicationFactor::new(0).unwrap()
}

const fn message_delayer_settings() -> MessageDelayerSettings {
    MessageDelayerSettings {
        maximum_release_delay_in_rounds: NonZeroU64::new(6).unwrap(),
    }
}

pub const SETTINGS_HEX: &str = "
    0100000000000000 0200000000000000 0300000000000000 0400000000000000 05000000 0d000000 0000000000000000
    0600000000000000 07000000 08000000 0900000000000000 0a000000000000000000000000000000
    0b00000000000000 0c00000000000000
";
const COMMON_SETTINGS_HEX: &str = "
    0100000000000000 0200000000000000 0300000000000000 0400000000000000
";
const CORE_SETTINGS_HEX: &str = "
    05000000 0d000000 0000000000000000 0600000000000000 07000000 08000000 0900000000000000
    0a000000000000000000000000000000 0b00000000000000 0c00000000000000
";
const SCHEDULER_SETTINGS_HEX: &str = "05000000 0d000000 0000000000000000 0600000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
codec_fixtures!(CommonSettings, encode_only, fixture_common_settings() => COMMON_SETTINGS_HEX);
codec_fixtures!(MinimumNetworkSize, encode_only, fixture_minimum_network_size() => "0200000000000000");
codec_fixtures!(CoreSettings, encode_only, fixture_core_settings() => CORE_SETTINGS_HEX);
codec_fixtures!(SchedulerSettings, encode_only, scheduler_settings() => SCHEDULER_SETTINGS_HEX);
codec_fixtures!(CoverTrafficSettings, encode_only, cover_traffic_settings() => "050000000d0000000000000000000000");
codec_fixtures!(CoverReplicationFactor, encode_only, fixture_cover_replication_factor() => "0000000000000000");
codec_fixtures!(MessageDelayerSettings, encode_only, message_delayer_settings() => "0600000000000000");
