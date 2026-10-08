//! The canonical encoding of the Blend v1 era parameters.

use core::num::{NonZeroU32, NonZeroU64, NonZeroU128};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_utils::math::PositiveF64;

use crate::config::deployment::parameters::blend::v1::{
    CommonSettings, CoreSettings, CoverTrafficSettings, MessageDelayerSettings, MinimumNetworkSize,
    SchedulerSettings, Settings,
};

impl BinaryEncode for Settings {
    fn encoded_length(&self) -> usize {
        let Self { common, core } = self;
        common.encoded_length() + core.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self { common, core } = self;
        common.encode_into(out);
        core.encode_into(out);
    }
}

impl BinaryEncode for CommonSettings {
    fn encoded_length(&self) -> usize {
        let Self {
            num_blend_layers,
            minimum_network_size,
            network_absorption_in_rounds,
            data_replication_factor,
        } = self;

        num_blend_layers.encoded_length()
            + minimum_network_size.encoded_length()
            + network_absorption_in_rounds.encoded_length()
            + data_replication_factor.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            num_blend_layers,
            minimum_network_size,
            network_absorption_in_rounds,
            data_replication_factor,
        } = self;
        num_blend_layers.encode_into(out);
        minimum_network_size.encode_into(out);
        network_absorption_in_rounds.encode_into(out);
        data_replication_factor.encode_into(out);
    }
}

// A minimum network size: the size it wraps.
impl BinaryEncode for MinimumNetworkSize {
    fn encoded_length(&self) -> usize {
        self.into_inner().encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.into_inner().encode_into(out);
    }
}

impl BinaryEncode for CoreSettings {
    fn encoded_length(&self) -> usize {
        let Self {
            scheduler,
            target_peering_degree,
            verification_rate_per_second,
            edge_node_send_deadline_in_rounds,
            core_handshake_deadline_in_rounds,
            activity_threshold_sensitivity,
        } = self;

        scheduler.encoded_length()
            + target_peering_degree.encoded_length()
            + verification_rate_per_second.encoded_length()
            + edge_node_send_deadline_in_rounds.encoded_length()
            + core_handshake_deadline_in_rounds.encoded_length()
            + activity_threshold_sensitivity.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            scheduler,
            target_peering_degree,
            verification_rate_per_second,
            edge_node_send_deadline_in_rounds,
            core_handshake_deadline_in_rounds,
            activity_threshold_sensitivity,
        } = self;
        scheduler.encode_into(out);
        target_peering_degree.encode_into(out);
        verification_rate_per_second.encode_into(out);
        edge_node_send_deadline_in_rounds.encode_into(out);
        core_handshake_deadline_in_rounds.encode_into(out);
        activity_threshold_sensitivity.encode_into(out);
    }
}

impl BinaryEncode for SchedulerSettings {
    fn encoded_length(&self) -> usize {
        let Self { cover, delayer } = self;

        cover.encoded_length() + delayer.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self { cover, delayer } = self;
        cover.encode_into(out);
        delayer.encode_into(out);
    }
}

impl BinaryEncode for CoverTrafficSettings {
    fn encoded_length(&self) -> usize {
        let Self {
            message_frequency_per_round,
        } = self;

        message_frequency_per_round.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            message_frequency_per_round,
        } = self;
        message_frequency_per_round.encode_into(out);
    }
}

impl BinaryEncode for MessageDelayerSettings {
    fn encoded_length(&self) -> usize {
        let Self {
            maximum_release_delay_in_rounds,
        } = self;

        maximum_release_delay_in_rounds.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            maximum_release_delay_in_rounds,
        } = self;
        maximum_release_delay_in_rounds.encode_into(out);
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

fn fixture_core_settings() -> CoreSettings {
    CoreSettings {
        scheduler: scheduler_settings(),
        target_peering_degree: NonZeroU32::new(7).unwrap(),
        verification_rate_per_second: NonZeroU32::new(8).unwrap(),
        edge_node_send_deadline_in_rounds: NonZeroU64::new(9).unwrap(),
        core_handshake_deadline_in_rounds: NonZeroU128::new(10).unwrap(),
        activity_threshold_sensitivity: 11,
    }
}

fn scheduler_settings() -> SchedulerSettings {
    SchedulerSettings {
        cover: cover_traffic_settings(),
        delayer: message_delayer_settings(),
    }
}

fn cover_traffic_settings() -> CoverTrafficSettings {
    CoverTrafficSettings {
        message_frequency_per_round: PositiveF64::try_from(5.0).unwrap(),
    }
}

const fn message_delayer_settings() -> MessageDelayerSettings {
    MessageDelayerSettings {
        maximum_release_delay_in_rounds: NonZeroU64::new(6).unwrap(),
    }
}

pub const SETTINGS_HEX: &str = "
    0100000000000000 0200000000000000 0300000000000000 0400000000000000 0000000000001440
    0600000000000000 07000000 08000000 0900000000000000 0a000000000000000000000000000000
    0b00000000000000
";
const COMMON_SETTINGS_HEX: &str = "
    0100000000000000 0200000000000000 0300000000000000 0400000000000000
";
const CORE_SETTINGS_HEX: &str = "
    0000000000001440 0600000000000000 07000000 08000000 0900000000000000
    0a000000000000000000000000000000 0b00000000000000
";
const SCHEDULER_SETTINGS_HEX: &str = "0000000000001440 0600000000000000";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
codec_fixtures!(CommonSettings, encode_only, fixture_common_settings() => COMMON_SETTINGS_HEX);
codec_fixtures!(MinimumNetworkSize, encode_only, fixture_minimum_network_size() => "0200000000000000");
codec_fixtures!(CoreSettings, encode_only, fixture_core_settings() => CORE_SETTINGS_HEX);
codec_fixtures!(SchedulerSettings, encode_only, scheduler_settings() => SCHEDULER_SETTINGS_HEX);
codec_fixtures!(CoverTrafficSettings, encode_only, cover_traffic_settings() => "0000000000001440");
codec_fixtures!(MessageDelayerSettings, encode_only, message_delayer_settings() => "0600000000000000");
