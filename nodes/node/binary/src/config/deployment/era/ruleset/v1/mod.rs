//! The ruleset of the chain releases 0.3.1 to 0.3.6 started: its parameters
//! in the layout those releases declared them in, which the digest of its era
//! commits to.
//!
//! Those releases fixed in code what the next ruleset makes parameters of an
//! era: the limits of a block, the gas of each Operation, the markets and the
//! block rewards. The rest of the node reads an era's parameters in the next
//! ruleset's layout, so the parameters declared here are converted to it once,
//! as they are read, with the values those releases fixed.

use core::num::{NonZero, NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128};

use lb_binary_codec::canonical::{BinaryEncode, codec_fixtures};
use lb_core::{
    mantle::{gas::GasProfile, ops::pow::PowReward, transactions::ConsensusExcludedKeys},
    sdp::{InactivityPeriod, MinStake, ServiceType},
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_utils::math::{NonNegativeF64, PositiveRatio};
use serde::{Deserialize, Serialize};
use strum::EnumCount as _;

use super::v2;
use crate::config::deployment::{InvalidDeployment, ensure};

/// The number the releases that ran this ruleset wrote ahead of its
/// parameters in an era's digest.
pub(super) const VERSION: u16 = 1;

/// `M`: the messages a core connection carries from cover traffic alone in one
/// observation window. A constant of the releases that ran this ruleset.
const COVER_MESSAGES_PER_OBSERVATION_WINDOW: u64 = 90;
/// The most uncles a block may reference.
const MAX_UNCLES: u8 = 4;
/// The most transactions a block may hold.
const MAX_TRANSACTIONS: NonZeroU16 = NonZeroU16::new(1024).unwrap();
/// The most bytes a block's transactions may take: 2 MiB.
const MAX_TRANSACTIONS_SIZE: u32 = 2 * 1024 * 1024;
/// The values those releases fixed in code that are not single numbers.
const CONSTANTS: &str = include_str!("constants.yaml");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Constants {
    execution_market: v2::cryptarchia::ExecutionMarketConfig,
    storage_market: v2::cryptarchia::StorageMarketConfig,
    block_rewards: v2::cryptarchia::BlockRewardsConfig,
    gas_profile: GasProfile,
}

/// The parameters of an era of this ruleset: as declared, and converted to the
/// next ruleset's layout.
///
/// (De)serialized as declared, and converted as it is read: parameters that do
/// not convert are refused.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(try_from = "Declared", into = "Declared")]
pub struct Parameters {
    declared: Declared,
    current: v2::Parameters,
}

impl Parameters {
    /// The parameters as declared, which the era's digest commits to.
    #[must_use]
    pub const fn declared(&self) -> &Declared {
        &self.declared
    }

    /// The parameters in the next ruleset's layout, with the values the
    /// releases that ran this ruleset fixed in code.
    #[must_use]
    pub const fn as_current(&self) -> &v2::Parameters {
        &self.current
    }

    /// The `PoW` reward pool the chain starts with: a genesis parameter the
    /// genesis block of a chain of this ruleset does not inscribe.
    #[must_use]
    pub const fn pow_reward_pool(&self) -> PowReward {
        self.declared
            .cryptarchia
            .pow_config
            .reward
            .reward_pool_genesis
    }

    /// The keys excluded from consensus, the faucet's if there is one: a
    /// genesis parameter the genesis block of a chain of this ruleset does not
    /// inscribe.
    #[must_use]
    pub fn consensus_excluded_keys(&self) -> ConsensusExcludedKeys {
        self.declared
            .cryptarchia
            .faucet_pk
            .map_or_else(ConsensusExcludedKeys::empty, ConsensusExcludedKeys::from)
    }
}

impl TryFrom<Declared> for Parameters {
    type Error = InvalidDeployment;

    fn try_from(declared: Declared) -> Result<Self, Self::Error> {
        let current = declared.to_current()?;
        Ok(Self { declared, current })
    }
}

impl From<Parameters> for Declared {
    fn from(parameters: Parameters) -> Self {
        parameters.declared
    }
}

/// The parameters of an era as releases 0.3.1 to 0.3.6 declared them.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Declared {
    pub blend: BlendSettings,
    pub cryptarchia: CryptarchiaSettings,
    pub time: v2::time::Settings,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct BlendSettings {
    pub common: v2::blend::CommonSettings,
    pub core: BlendCoreSettings,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct BlendCoreSettings {
    pub scheduler: BlendSchedulerSettings,
    pub target_peering_degree: NonZeroU32,
    pub verification_rate_per_second: NonZeroU32,
    pub edge_node_send_deadline_in_rounds: NonZeroU64,
    pub core_handshake_deadline_in_rounds: NonZeroU128,
    pub activity_threshold_sensitivity: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct BlendSchedulerSettings {
    pub cover: BlendCoverTrafficSettings,
    pub delayer: v2::blend::MessageDelayerSettings,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct BlendCoverTrafficSettings {
    /// A ratio here, which those releases declared as the float it encodes
    /// as.
    pub message_frequency_per_round: PositiveRatio,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct CryptarchiaSettings {
    pub epoch_config: v2::cryptarchia::EpochConfig,
    pub security_param: NonZeroU32,
    pub slot_activation_coeff: v2::cryptarchia::SlotActivationCoeff,
    pub learning_rate: NonNegativeF64,
    pub uncle_reference_window_in_block: NonZeroU32,
    pub sdp_config: SdpConfig,
    /// The key whose notes take no part in consensus.
    #[serde(default)]
    pub faucet_pk: Option<ZkPublicKey>,
    pub pow_config: PoWConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct SdpConfig {
    /// The parameters of each service type, in the order [`ServiceType`]
    /// declares them in.
    pub service_params: [ServiceParameters; ServiceType::COUNT],
    pub min_stake: LegacyMinStake,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct ServiceParameters {
    pub inactivity_period: InactivityPeriod,
    /// Never read, but part of the era's digest.
    pub epoch: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct LegacyMinStake {
    pub threshold: u64,
    /// Never read, but part of the era's digest.
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct PoWConfig {
    pub blend: v2::cryptarchia::BlendPoWConfig,
    pub reward: RewardPoWConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct RewardPoWConfig {
    /// The initial balance of the reward pool, which the next ruleset reads
    /// from the genesis block instead.
    pub reward_pool_genesis: PowReward,
    /// The initial per-claim reward, which must be the one the pool and the
    /// payout rate give: the next ruleset derives it.
    pub epoch_reward_genesis: PowReward,
    pub minimum_difficulty: lb_groth16::ModulusShift,
    pub ema_smoothing_factor: u64,
    pub ema_smoothing_precision: NonZeroU64,
    pub target_claims_per_block: NonZeroU64,
    pub rate_num: u64,
    pub rate_den: NonZeroU64,
    /// The same as [`Self::target_claims_per_block`], which the next ruleset
    /// keeps alone.
    pub target_claim_per_block: NonZeroU64,
    pub pow_share: u64,
    pub share_den: NonZeroU64,
    pub slot_window: NonZeroU64,
}

impl Declared {
    /// These parameters in the next ruleset's layout.
    fn to_current(&self) -> Result<v2::Parameters, InvalidDeployment> {
        let Constants {
            execution_market,
            storage_market,
            block_rewards,
            gas_profile,
        } = serde_yaml::from_str(CONSTANTS).expect("The legacy constants are valid.");
        let Self {
            blend,
            cryptarchia,
            time,
        } = self;

        let reward = &cryptarchia.pow_config.reward;
        ensure!(
            reward.target_claim_per_block == reward.target_claims_per_block,
            "The two targets of PoW reward claims per block, {} and {}, must be the same.",
            reward.target_claim_per_block,
            reward.target_claims_per_block
        );
        let cryptarchia = v2::cryptarchia::Settings {
            epoch_config: cryptarchia.epoch_config,
            security_param: cryptarchia.security_param,
            slot_activation_coeff: cryptarchia.slot_activation_coeff,
            learning_rate: cryptarchia.learning_rate,
            uncle_reference_window_in_block: cryptarchia.uncle_reference_window_in_block.get(),
            max_uncles: MAX_UNCLES,
            max_transactions: MAX_TRANSACTIONS,
            max_transactions_size: MAX_TRANSACTIONS_SIZE,
            sdp_config: v2::cryptarchia::SdpConfig {
                service_params: cryptarchia.sdp_config.service_params.clone().map(
                    |ServiceParameters {
                         inactivity_period, ..
                     }| v2::cryptarchia::ServiceParameters {
                        inactivity_period,
                    },
                ),
                min_stake: MinStake {
                    threshold: cryptarchia.sdp_config.min_stake.threshold,
                },
            },
            pow_config: v2::cryptarchia::PoWConfig {
                blend: cryptarchia.pow_config.blend.clone(),
                reward: v2::cryptarchia::RewardPoWConfig {
                    minimum_difficulty: reward.minimum_difficulty,
                    ema_smoothing_factor: reward.ema_smoothing_factor,
                    ema_smoothing_precision: reward.ema_smoothing_precision,
                    target_claims_per_block: reward.target_claims_per_block,
                    rate_num: reward.rate_num,
                    rate_den: reward.rate_den,
                    pow_share: reward.pow_share,
                    share_den: reward.share_den,
                    slot_window: reward.slot_window,
                },
            },
            execution_market,
            storage_market,
            block_rewards,
            gas_profile,
        };
        // The next ruleset derives the first epoch's reward from the pool,
        // where this one declares it.
        let claims_per_epoch = u128::from(reward.rate_den.get())
            * u128::from(reward.target_claims_per_block.get())
            * u128::from(cryptarchia.expected_blocks_per_epoch().get());
        let derived_epoch_reward =
            u128::from(reward.reward_pool_genesis) * u128::from(reward.rate_num) / claims_per_epoch;
        ensure!(
            u128::from(reward.epoch_reward_genesis) == derived_epoch_reward,
            "The genesis PoW reward of an epoch, {}, must be the one its pool and payout rate \
             give, {derived_epoch_reward}.",
            reward.epoch_reward_genesis
        );

        let current = v2::Parameters {
            blend: blend.to_current()?,
            cryptarchia,
            time: time.clone(),
        };
        let (window, legacy_window) = (
            current.blend.rounds_per_observation_window().get(),
            blend.rounds_per_observation_window(),
        );
        ensure!(
            window == legacy_window,
            "The Blend observation window of these parameters, {legacy_window} rounds, would be \
             {window} rounds under the next ruleset."
        );
        Ok(current)
    }
}

impl BlendSettings {
    fn to_current(&self) -> Result<v2::blend::Settings, InvalidDeployment> {
        let Self { common, core } = self;
        // The next ruleset sizes the observation window from the deliveries
        // expected of the neighbour with the fewest connections, `λ`, where
        // this one sized it from `M = λ · (Φ_CC − 1) / (Φ_CC − 2)` directly.
        let peering_degree = NonZeroU64::from(core.target_peering_degree).get();
        let expected_deliveries = peering_degree
            .checked_sub(2)
            .map(|fewest_others| COVER_MESSAGES_PER_OBSERVATION_WINDOW * fewest_others)
            .filter(|scaled| scaled % (peering_degree - 1) == 0)
            .and_then(|scaled| NonZero::new(scaled / (peering_degree - 1)));
        let Some(expected_deliveries_per_observation_window) = expected_deliveries else {
            return Err(InvalidDeployment::new(format!(
                "A Blend peering degree of {peering_degree} gives the observation window of \
                 {COVER_MESSAGES_PER_OBSERVATION_WINDOW} cover messages no whole number of \
                 expected deliveries."
            )));
        };
        Ok(v2::blend::Settings {
            common: common.clone(),
            core: v2::blend::CoreSettings {
                scheduler: v2::blend::SchedulerSettings {
                    cover: v2::blend::CoverTrafficSettings {
                        message_frequency_per_round: core
                            .scheduler
                            .cover
                            .message_frequency_per_round,
                        replication_factor: v2::blend::CoverReplicationFactor::try_from(0)
                            .expect("No cover replication is a valid factor."),
                    },
                    delayer: core.scheduler.delayer.clone(),
                },
                target_peering_degree: core.target_peering_degree,
                verification_rate_per_second: core.verification_rate_per_second,
                edge_node_send_deadline_in_rounds: core.edge_node_send_deadline_in_rounds,
                core_handshake_deadline_in_rounds: core.core_handshake_deadline_in_rounds,
                expected_deliveries_per_observation_window,
                activity_threshold_sensitivity: core.activity_threshold_sensitivity,
            },
        })
    }

    /// `W = ⌈M / (F_C · β_C)⌉`, as the releases that ran this ruleset computed
    /// it, in floats.
    fn rounds_per_observation_window(&self) -> u128 {
        let cover_messages_per_round =
            self.message_frequency_per_round() * self.common.num_blend_layers.get() as f64;
        (COVER_MESSAGES_PER_OBSERVATION_WINDOW as f64 / cover_messages_per_round).ceil() as u128
    }

    const fn message_frequency_per_round(&self) -> f64 {
        self.core
            .scheduler
            .cover
            .message_frequency_per_round
            .get()
            .as_f64()
    }
}

/// The preimage of the era's digest, after the ruleset's number: byte for
/// byte what releases 0.3.1 to 0.3.6 encoded their era parameters as. Each
/// field in the order those releases declared them, depth first: an integer
/// little-endian at its width, a float as its IEEE 754 bits, a ratio as its
/// numerator then its denominator, the service parameters as a map from the
/// byte of each service type, the faucet key as an optional value, and the
/// slot duration as its whole seconds then the nanoseconds past them.
impl BinaryEncode for Declared {
    fn encoded_length(&self) -> usize {
        // Only hashed once, as the deployment is read: encoded rather than
        // summed field by field. Not through `encode`, which reserves this
        // length first.
        let mut encoded = Vec::new();
        self.encode_into(&mut encoded);
        encoded.len()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            blend: BlendSettings { common, core },
            cryptarchia,
            time,
        } = self;

        common.num_blend_layers.encode_into(out);
        u64::from(NonZeroU64::from(common.minimum_network_size)).encode_into(out);
        common.network_absorption_in_rounds.encode_into(out);
        common.data_replication_factor.encode_into(out);
        self.blend
            .message_frequency_per_round()
            .to_bits()
            .encode_into(out);
        core.scheduler
            .delayer
            .maximum_release_delay_in_rounds
            .encode_into(out);
        core.target_peering_degree.encode_into(out);
        core.verification_rate_per_second.encode_into(out);
        core.edge_node_send_deadline_in_rounds.encode_into(out);
        core.core_handshake_deadline_in_rounds.encode_into(out);
        core.activity_threshold_sensitivity.encode_into(out);

        let CryptarchiaSettings {
            epoch_config,
            security_param,
            slot_activation_coeff,
            learning_rate,
            uncle_reference_window_in_block,
            sdp_config,
            faucet_pk,
            pow_config: PoWConfig { blend, reward },
        } = cryptarchia;
        epoch_config.encode_into(out);
        security_param.encode_into(out);
        slot_activation_coeff.encode_into(out);
        learning_rate.get().to_bits().encode_into(out);
        uncle_reference_window_in_block.encode_into(out);
        (ServiceType::COUNT as u32).encode_into(out);
        for (service_type, parameters) in (0u8..).zip(&sdp_config.service_params) {
            service_type.encode_into(out);
            parameters.inactivity_period.encode_into(out);
            parameters.epoch.encode_into(out);
        }
        sdp_config.min_stake.threshold.encode_into(out);
        sdp_config.min_stake.timestamp.encode_into(out);
        faucet_pk.encode_into(out);
        blend.encode_into(out);
        let RewardPoWConfig {
            reward_pool_genesis,
            epoch_reward_genesis,
            minimum_difficulty,
            ema_smoothing_factor,
            ema_smoothing_precision,
            target_claims_per_block,
            rate_num,
            rate_den,
            target_claim_per_block,
            pow_share,
            share_den,
            slot_window,
        } = reward;
        reward_pool_genesis.encode_into(out);
        epoch_reward_genesis.encode_into(out);
        minimum_difficulty.encode_into(out);
        ema_smoothing_factor.encode_into(out);
        ema_smoothing_precision.encode_into(out);
        target_claims_per_block.encode_into(out);
        rate_num.encode_into(out);
        rate_den.encode_into(out);
        target_claim_per_block.encode_into(out);
        pow_share.encode_into(out);
        share_den.encode_into(out);
        slot_window.encode_into(out);

        time.slot_duration_in_seconds.encode_into(out);
        // The nanoseconds past the whole seconds.
        0u32.encode_into(out);
    }
}

/// The genesis era of the testnet release 0.3.1 started, as declared.
fn testnet_genesis_era() -> Declared {
    const ERA: &str = include_str!("../../../testnet/era/schedule/0.yaml");
    match serde_yaml::from_str(ERA).expect("The testnet's genesis era is valid.") {
        super::EraRuleset::V1(parameters) => parameters.declared,
        super::EraRuleset::V2(_) => unreachable!("The testnet's genesis era is a legacy one."),
    }
}

/// What release 0.3.6 encodes the testnet's genesis era as, after the
/// ruleset's number: printed by that release, not by this one.
const TESTNET_GENESIS_ERA_HEX: &str = "
    0100000000000000 0200000000000000 0200000000000000 0100000000000000 000000000000f03f
    0100000000000000 04000000 9c000000 0100000000000000 02000000000000000000000000000000
    0100000000000000
    03 03 04 e8030000 01000000 1e000000 000000000000f03f 0c000000
    01000000 00 02000000 00000000 00ca9a3b00000000 0000000000000000
    01 c2a6a4a0981d5bdcf8ddeb8d7934fd8c5510efeb1053f613b45871670b6f7b19
    13000000 0c00000000000000 0200000000000000 01000000 01000000
    003086aa750b0000 80841e0000000000 13000000 0900000000000000 0a00000000000000
    0a00000000000000 0100000000000000 3f00000000000000 0a00000000000000 0a00000000000000
    6400000000000000 5802000000000000
    0100000000000000 00000000
";

codec_fixtures!(Declared, encode_only, testnet_genesis_era() => TESTNET_GENESIS_ERA_HEX);
