//! The fixtures of the canonical encoding of the cryptarchia era parameters.

use core::num::{NonZero, NonZeroU32, NonZeroU64};

use lb_binary_codec::canonical::codec_fixtures;
use lb_core::sdp::{InactivityPeriod, MinStake};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::ModulusShift;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio, ProperFraction};

use super::{
    BlendPoWConfig, EpochConfig, ExecutionMarketConfig, MaxGasPerBlock, PoWConfig, RewardPoWConfig,
    SdpConfig, ServiceParameters, Settings, SlotActivationCoeff, StorageMarketConfig,
};

pub fn fixture_settings() -> Settings {
    Settings {
        epoch_config: fixture_epoch_config(),
        security_param: NonZeroU32::new(15).unwrap(),
        slot_activation_coeff: fixture_slot_activation_coeff(),
        learning_rate: NonNegativeF64::try_from(18.0).unwrap(),
        uncle_reference_window_in_block: 19,
        max_uncles: 24,
        sdp_config: fixture_sdp_config(),
        pow_config: fixture_pow_config(),
        execution_market: ExecutionMarketConfig {
            max_gas_per_block: MaxGasPerBlock::new(43).unwrap(),
            fee_adjustment_rate: ProperFraction::new(44, 45.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(46, 47.try_into().unwrap()).unwrap(),
        },
        storage_market: StorageMarketConfig {
            max_adjustment_factor: ProperFraction::new(48, 49.try_into().unwrap()).unwrap(),
            ema_smoothing_factor: ProperFraction::new(50, 51.try_into().unwrap()).unwrap(),
        },
    }
}

fn fixture_slot_activation_coeff() -> SlotActivationCoeff {
    NonNegativeRatio::new(16, NonZeroU32::new(17).unwrap())
        .try_into()
        .unwrap()
}

const fn fixture_epoch_config() -> EpochConfig {
    EpochConfig {
        epoch_stake_distribution_stabilization: NonZero::new(12).unwrap(),
        epoch_period_nonce_buffer: NonZero::new(13).unwrap(),
        epoch_period_nonce_stabilization: NonZero::new(14).unwrap(),
    }
}

fn fixture_sdp_config() -> SdpConfig {
    SdpConfig {
        service_params: [fixture_service_parameters()],
        min_stake: MinStake {
            threshold: 22,
            timestamp: 23,
        },
    }
}

fn fixture_service_parameters() -> ServiceParameters {
    ServiceParameters {
        inactivity_period: InactivityPeriod::new(Epoch::new(20)).unwrap(),
        epoch: Epoch::new(21),
    }
}

const fn fixture_pow_config() -> PoWConfig {
    PoWConfig {
        blend: fixture_blend_pow_config(),
        reward: RewardPoWConfig {
            minimum_difficulty: ModulusShift::new::<32>(),
            ema_smoothing_factor: 33,
            ema_smoothing_precision: NonZeroU64::new(34).unwrap(),
            target_claims_per_block: 35,
            rate_num: 36,
            rate_den: NonZeroU64::new(37).unwrap(),
            target_claim_per_block: NonZeroU64::new(38).unwrap(),
            pow_share: 39,
            share_den: NonZeroU64::new(40).unwrap(),
            slot_window: NonZeroU64::new(41).unwrap(),
        },
    }
}

const fn fixture_blend_pow_config() -> BlendPoWConfig {
    BlendPoWConfig {
        base_difficulty: ModulusShift::new::<25>(),
        target_transactions_per_block: NonZeroU64::new(26).unwrap(),
        max_step: NonZeroU64::new(27).unwrap(),
        damping_num: NonZeroU32::new(28).unwrap(),
        damping_den_offset: 29,
    }
}

pub const SETTINGS_HEX: &str = "
    0c 0d 0e 0f000000 10000000 11000000 0000000000003240 13000000 18 14000000 15000000
    1600000000000000 1700000000000000 19000000 1a00000000000000
    1b00000000000000 1c000000 1d000000 20000000
    2100000000000000 2200000000000000 2300000000000000 2400000000000000 2500000000000000
    2600000000000000 2700000000000000 2800000000000000 2900000000000000
    2b00000000000000 2c000000 2d000000 2e000000 2f000000 30000000 31000000 32000000 33000000
";
const SDP_CONFIG_HEX: &str = "14000000 15000000 1600000000000000 1700000000000000";
const POW_CONFIG_HEX: &str = "
    19000000 1a00000000000000 1b00000000000000 1c000000 1d000000 20000000 2100000000000000 2200000000000000 2300000000000000
    2400000000000000 2500000000000000 2600000000000000 2700000000000000 2800000000000000
    2900000000000000
";
const BLEND_POW_CONFIG_HEX: &str = "
    19000000 1a00000000000000 1b00000000000000 1c000000 1d000000
";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
codec_fixtures!(SlotActivationCoeff, encode_only, fixture_slot_activation_coeff() => "1000000011000000");
codec_fixtures!(EpochConfig, encode_only, fixture_epoch_config() => "0c0d0e");
codec_fixtures!(SdpConfig, encode_only, fixture_sdp_config() => SDP_CONFIG_HEX);
codec_fixtures!(ServiceParameters, encode_only, fixture_service_parameters() => "1400000015000000");
codec_fixtures!(PoWConfig, encode_only, fixture_pow_config() => POW_CONFIG_HEX);
codec_fixtures!(BlendPoWConfig, encode_only, fixture_blend_pow_config() => BLEND_POW_CONFIG_HEX);
