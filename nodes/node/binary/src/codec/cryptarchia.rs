//! The canonical encoding of the cryptarchia era parameters.

use core::num::{NonZero, NonZeroU32, NonZeroU64};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_core::sdp::{InactivityPeriod, MinStake, ServiceType};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::{Fr, ModulusShift};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_utils::math::{NonNegativeF64, NonNegativeRatio};

use crate::config::cryptarchia::deployment::{
    BlendPoWConfig, EpochConfig, PoWConfig, RewardPoWConfig, SdpConfig, ServiceParameters, Settings,
};

impl BinaryEncode for Settings {
    fn encoded_length(&self) -> usize {
        let Self {
            epoch_config,
            security_param,
            slot_activation_coeff,
            learning_rate,
            uncle_reference_window_in_block,
            sdp_config,
            faucet_pk,
            pow_config,
        } = self;

        epoch_config.encoded_length()
            + security_param.encoded_length()
            + slot_activation_coeff.encoded_length()
            + learning_rate.encoded_length()
            + uncle_reference_window_in_block.encoded_length()
            + sdp_config.encoded_length()
            + faucet_pk.encoded_length()
            + pow_config.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            epoch_config,
            security_param,
            slot_activation_coeff,
            learning_rate,
            uncle_reference_window_in_block,
            sdp_config,
            faucet_pk,
            pow_config,
        } = self;

        epoch_config.encode_into(out);
        security_param.encode_into(out);
        slot_activation_coeff.encode_into(out);
        learning_rate.encode_into(out);
        uncle_reference_window_in_block.encode_into(out);
        sdp_config.encode_into(out);
        faucet_pk.encode_into(out);
        pow_config.encode_into(out);
    }
}

impl BinaryEncode for EpochConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            epoch_stake_distribution_stabilization,
            epoch_period_nonce_buffer,
            epoch_period_nonce_stabilization,
        } = self;

        epoch_stake_distribution_stabilization.encoded_length()
            + epoch_period_nonce_buffer.encoded_length()
            + epoch_period_nonce_stabilization.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            epoch_stake_distribution_stabilization,
            epoch_period_nonce_buffer,
            epoch_period_nonce_stabilization,
        } = self;

        epoch_stake_distribution_stabilization.encode_into(out);
        epoch_period_nonce_buffer.encode_into(out);
        epoch_period_nonce_stabilization.encode_into(out);
    }
}

impl BinaryEncode for SdpConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            service_params,
            min_stake,
        } = self;

        service_params.encoded_length() + min_stake.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            service_params,
            min_stake,
        } = self;

        service_params.encode_into(out);
        min_stake.encode_into(out);
    }
}

impl BinaryEncode for ServiceParameters {
    fn encoded_length(&self) -> usize {
        let Self {
            inactivity_period,
            epoch,
        } = self;

        inactivity_period.encoded_length() + epoch.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            inactivity_period,
            epoch,
        } = self;

        inactivity_period.encode_into(out);
        epoch.encode_into(out);
    }
}

impl BinaryEncode for PoWConfig {
    fn encoded_length(&self) -> usize {
        let Self { blend, reward } = self;

        blend.encoded_length() + reward.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self { blend, reward } = self;

        blend.encode_into(out);
        reward.encode_into(out);
    }
}

impl BinaryEncode for BlendPoWConfig {
    fn encoded_length(&self) -> usize {
        let Self {
            base_difficulty,
            target_transactions_per_block,
            max_step,
            damping_num,
            damping_den_offset,
        } = self;

        base_difficulty.encoded_length()
            + target_transactions_per_block.encoded_length()
            + max_step.encoded_length()
            + damping_num.encoded_length()
            + damping_den_offset.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            base_difficulty,
            target_transactions_per_block,
            max_step,
            damping_num,
            damping_den_offset,
        } = self;

        base_difficulty.encode_into(out);
        target_transactions_per_block.encode_into(out);
        max_step.encode_into(out);
        damping_num.encode_into(out);
        damping_den_offset.encode_into(out);
    }
}

pub(super) fn fixture_settings() -> Settings {
    Settings {
        epoch_config: fixture_epoch_config(),
        security_param: NonZeroU32::new(15).unwrap(),
        slot_activation_coeff: NonNegativeRatio::new(16, NonZeroU32::new(17).unwrap()),
        learning_rate: NonNegativeF64::try_from(18.0).unwrap(),
        uncle_reference_window_in_block: NonZeroU32::new(19).unwrap(),
        sdp_config: fixture_sdp_config(),
        faucet_pk: Some(ZkPublicKey::new(Fr::from(24u64))),
        pow_config: fixture_pow_config(),
    }
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
        service_params: (ServiceType::BlendNetwork, fixture_service_parameters()).into(),
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
            reward_pool_genesis: 30,
            epoch_reward_genesis: 31,
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

pub(super) const SETTINGS_HEX: &str = "
    0c 0d 0e 0f000000 10000000 11000000 0000000000003240 13000000 01000000 00 14000000
    15000000 1600000000000000 1700000000000000 01
    1800000000000000000000000000000000000000000000000000000000000000 19000000 1a00000000000000
    1b00000000000000 1c000000 1d000000 1e00000000000000 1f00000000000000 20000000
    2100000000000000 2200000000000000 2300000000000000 2400000000000000 2500000000000000
    2600000000000000 2700000000000000 2800000000000000 2900000000000000
";
const SDP_CONFIG_HEX: &str = "
    01000000 00 14000000 15000000 1600000000000000 1700000000000000
";
const POW_CONFIG_HEX: &str = "
    19000000 1a00000000000000 1b00000000000000 1c000000 1d000000 1e00000000000000
    1f00000000000000 20000000 2100000000000000 2200000000000000 2300000000000000
    2400000000000000 2500000000000000 2600000000000000 2700000000000000 2800000000000000
    2900000000000000
";
const BLEND_POW_CONFIG_HEX: &str = "
    19000000 1a00000000000000 1b00000000000000 1c000000 1d000000
";

codec_fixtures!(Settings, encode_only, fixture_settings() => SETTINGS_HEX);
codec_fixtures!(EpochConfig, encode_only, fixture_epoch_config() => "0c0d0e");
codec_fixtures!(SdpConfig, encode_only, fixture_sdp_config() => SDP_CONFIG_HEX);
codec_fixtures!(ServiceParameters, encode_only, fixture_service_parameters() => "1400000015000000");
codec_fixtures!(PoWConfig, encode_only, fixture_pow_config() => POW_CONFIG_HEX);
codec_fixtures!(BlendPoWConfig, encode_only, fixture_blend_pow_config() => BLEND_POW_CONFIG_HEX);
