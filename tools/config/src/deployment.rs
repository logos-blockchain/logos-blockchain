use core::num::{NonZero, NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128};

use lb_core::{
    block::genesis::GenesisBlock,
    mantle::gas::{Gas, GasProfile},
    sdp::NumberOfEpochs,
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::ModulusShift;
use lb_node::config::deployment::{
    DeploymentSettings, EraDeclarations,
    era::ruleset::{
        EraRuleset, v1,
        v1::{
            blend::{
                CommonSettings as BlendCommonSettings, CoreSettings as BlendCoreSettings,
                CoverReplicationFactor, CoverTrafficSettings, MessageDelayerSettings,
                MinimumNetworkSize, SchedulerSettings, Settings as BlendDeploymentSettings,
            },
            cryptarchia::{
                BlockRewardsConfig, EpochConfig, MaxGasPerBlock, ServiceParameters,
                Settings as CryptarchiaDeploymentSettings,
            },
            time::Settings as TimeDeploymentSettings,
        },
    },
};
use lb_utils::math::{NonNegativeRatio, PositiveF64, ProperFraction};

use crate::time::{CONSENSUS_SLOT_TIME_VAR, DEFAULT_SLOT_TIME_IN_SECS};

/// `T_E`: the rounds an edge node is given to send its message.
const BLEND_EDGE_NODE_SEND_DEADLINE_IN_ROUNDS: u64 = 1;
/// `T_H`: the rounds a core handshake is given to complete.
const BLEND_CORE_HANDSHAKE_DEADLINE_IN_ROUNDS: u128 = 2;
/// `λ`: the messages a neighbour is expected to deliver to a node in one
/// observation window, which sizes the window with the peering degree.
const BLEND_EXPECTED_DELIVERIES_PER_OBSERVATION_WINDOW: u64 = 60;
/// `V`: the messages per second the slowest targeted node can verify.
const BLEND_VERIFICATION_RATE_PER_SECOND: u32 = 156;
/// `η`: the rounds a message spends crossing the network at one hop.
const BLEND_NETWORK_ABSORPTION_IN_ROUNDS: u64 = 2;
const BLEND_PEERING_DEGREE: NonZeroU32 = NonZeroU32::new(4).unwrap();
const MINIMUM_BLEND_NETWORK_SIZE: u64 = 2;
const NUM_BLEND_LAYERS: u64 = 3;
const DATA_REPLICATION_FACTOR: u64 = 0;

const COVER_MESSAGE_FREQUENCY_PER_ROUND: f64 = 1.0;
const COVER_REPLICATION_FACTOR: CoverReplicationFactor = CoverReplicationFactor::new(0).unwrap();
const MAXIMUM_RELEASE_DELAY_IN_ROUNDS: u64 = 3;
const ACTIVITY_THRESHOLD_SENSITIVITY: u64 = 1;

const SECURITY_PARAM: u32 = 20;
const UNCLE_REFERENCE_WINDOW_IN_BLOCK: u32 = 12;
const MAX_UNCLES: u8 = 4;
const MAX_TRANSACTIONS: NonZeroU16 = NonZeroU16::new(1024).unwrap();
const MAX_TRANSACTIONS_SIZE: u32 = 2 * 1024 * 1024;
const SLOT_ACTIVATION_COEFF_NUMERATOR: u32 = 1;
const SLOT_ACTIVATION_COEFF_DENOMINATOR: u32 = 10;
const EPOCH_STAKE_DISTRIBUTION_STABILIZATION: u8 = 3;
const EPOCH_PERIOD_NONCE_BUFFER: u8 = 3;
const EPOCH_PERIOD_NONCE_STABILIZATION: u8 = 4;

const SDP_INACTIVITY_PERIOD: NumberOfEpochs = NumberOfEpochs::new(2);
const SDP_EPOCH: Epoch = Epoch::new(0);
const MIN_STAKE_THRESHOLD: u64 = 1;
const MIN_STAKE_TIMESTAMP: u64 = 0;
const LEARNING_RATE: f64 = 0.1;
const BLEND_POW_BASE_DIFFICULTY_EXPONENT: u32 = 19;
const BLEND_POW_TARGET_TXS_PER_BLOCK: u64 = 512;
const BLEND_POW_MAX_STEP: u64 = 2;
const BLEND_POW_DAMPING_NUM: u32 = 1;
const BLEND_POW_DAMPING_DEN_OFFSET: u32 = 1;

// Token-reward PoW parameters. Payout is disabled (`rate_num = 0`).
const REWARD_POW_MINIMUM_DIFFICULTY_EXPONENT: u32 = 19;
const REWARD_POW_EMA_SMOOTHING_FACTOR: u64 = 9;
const REWARD_POW_EMA_SMOOTHING_PRECISION: u64 = 10;
const REWARD_POW_TARGET_CLAIMS_PER_BLOCK: u64 = 100;
const REWARD_POW_RATE_NUM: u64 = 0;
const REWARD_POW_RATE_DEN: u64 = 1;
const REWARD_POW_TARGET_CLAIM_PER_BLOCK: u64 = 1;
const REWARD_POW_SHARE: u64 = 10;
const REWARD_POW_SHARE_DEN: u64 = 100;
const REWARD_POW_SLOT_WINDOW: u64 = 100;

/// `G_max`: the most execution gas a block may consume, whose half is the
/// target the execution base fee steers blocks towards.
const EXECUTION_MARKET_MAX_GAS_PER_BLOCK: MaxGasPerBlock = MaxGasPerBlock::new(3_193_460).unwrap();
/// `φ`: how fast the execution base fee follows demand.
const EXECUTION_MARKET_FEE_ADJUSTMENT_RATE: ProperFraction =
    ProperFraction::new(1, NonZeroU32::new(8).unwrap()).unwrap();
/// `q`: the weight of the previous average in the moving average of block
/// usage.
const EXECUTION_MARKET_EMA_SMOOTHING_FACTOR: ProperFraction =
    ProperFraction::new(9, NonZeroU32::new(10).unwrap()).unwrap();
/// `α`: the largest fraction by which the storage price moves in an epoch.
const STORAGE_MARKET_MAX_ADJUSTMENT_FACTOR: ProperFraction =
    ProperFraction::new(1, NonZeroU32::new(8).unwrap()).unwrap();
/// `β`: the weight of the latest epoch in the moving average of storage usage.
const STORAGE_MARKET_EMA_SMOOTHING_FACTOR: ProperFraction =
    ProperFraction::new(1, NonZeroU32::new(2).unwrap()).unwrap();
/// What each Operation costs, the channel ones per key of their threshold.
///
/// The schedule of the e2e deployment, which its clients price transactions
/// under.
pub const GAS_PROFILE: GasProfile = GasProfile {
    transfer: Gas::new(590),
    channel_config: Gas::new(56),
    channel_inscribe: Gas::new(56),
    channel_deposit: Gas::new(590),
    channel_withdraw: Gas::new(56),
    channel_transfer: Gas::new(56),
    sdp_declare: Gas::new(646),
    sdp_withdraw: Gas::new(590),
    sdp_active: Gas::new(590),
    leader_claim: Gas::new(580),
    claim_pow_reward: Gas::new(1),
};
/// The block rewards: the stake target, the scale of the emission's weight, the
/// weight of the pooled fees, a block's emission in full, and the Blend nodes'
/// share.
const BLOCK_REWARDS: BlockRewardsConfig = BlockRewardsConfig::new(
    3_000_000_000,
    NonZeroU64::new(120_000_000).unwrap(),
    10_512,
    NonNegativeRatio::new(62_500, NonZeroU32::new(657).unwrap()),
    ProperFraction::new(6, NonZeroU32::new(10).unwrap()).unwrap(),
)
.unwrap();

#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Deployment settings assembled in a single place for clarity."
)]
pub fn e2e_deployment_settings_with_genesis_block(
    genesis_block: &GenesisBlock,
) -> DeploymentSettings {
    let genesis_tx = genesis_block.genesis_tx();
    let slot_duration_in_secs = std::env::var(CONSENSUS_SLOT_TIME_VAR)
        .map_or(DEFAULT_SLOT_TIME_IN_SECS, |s| s.parse::<u64>().unwrap());

    DeploymentSettings::new(
        &EraDeclarations::new_genesis(EraRuleset::V1(v1::Parameters {
            blend: BlendDeploymentSettings {
                common: BlendCommonSettings {
                    minimum_network_size: MinimumNetworkSize::try_new(MINIMUM_BLEND_NETWORK_SIZE)
                        .expect("Minimum network size cannot be less than 2."),
                    num_blend_layers: NonZeroU64::try_from(NUM_BLEND_LAYERS)
                        .expect("Number of blend layers cannot be zero."),
                    network_absorption_in_rounds: NonZeroU64::try_from(
                        BLEND_NETWORK_ABSORPTION_IN_ROUNDS,
                    )
                    .expect("Network absorption cannot be zero."),
                    data_replication_factor: DATA_REPLICATION_FACTOR,
                },
                core: BlendCoreSettings {
                    target_peering_degree: BLEND_PEERING_DEGREE,
                    verification_rate_per_second: NonZeroU32::try_from(
                        BLEND_VERIFICATION_RATE_PER_SECOND,
                    )
                    .expect("Verification rate cannot be zero."),
                    edge_node_send_deadline_in_rounds: NonZeroU64::try_from(
                        BLEND_EDGE_NODE_SEND_DEADLINE_IN_ROUNDS,
                    )
                    .expect("`T_E` cannot be zero."),
                    core_handshake_deadline_in_rounds: NonZeroU128::try_from(
                        BLEND_CORE_HANDSHAKE_DEADLINE_IN_ROUNDS,
                    )
                    .expect("`T_H` cannot be zero."),
                    expected_deliveries_per_observation_window: NonZeroU64::try_from(
                        BLEND_EXPECTED_DELIVERIES_PER_OBSERVATION_WINDOW,
                    )
                    .expect("The expected deliveries per observation window cannot be zero."),
                    scheduler: SchedulerSettings {
                        cover: CoverTrafficSettings {
                            message_frequency_per_round: PositiveF64::try_from(
                                COVER_MESSAGE_FREQUENCY_PER_ROUND,
                            )
                            .expect("Message frequency per round must be positive."),
                            replication_factor: COVER_REPLICATION_FACTOR,
                        },
                        delayer: MessageDelayerSettings {
                            maximum_release_delay_in_rounds: NonZeroU64::try_from(
                                MAXIMUM_RELEASE_DELAY_IN_ROUNDS,
                            )
                            .expect("Maximum release delay between rounds cannot be zero."),
                        },
                    },
                    activity_threshold_sensitivity: ACTIVITY_THRESHOLD_SENSITIVITY,
                },
            },
            cryptarchia: CryptarchiaDeploymentSettings {
                security_param: NonZero::new(SECURITY_PARAM).unwrap(),
                uncle_reference_window_in_block: UNCLE_REFERENCE_WINDOW_IN_BLOCK,
                max_uncles: MAX_UNCLES,
                max_transactions: MAX_TRANSACTIONS,
                max_transactions_size: MAX_TRANSACTIONS_SIZE,
                slot_activation_coeff: NonNegativeRatio::new(
                    SLOT_ACTIVATION_COEFF_NUMERATOR,
                    NonZero::new(SLOT_ACTIVATION_COEFF_DENOMINATOR).unwrap(),
                )
                .try_into()
                .expect("The e2e slot activation coefficient is valid."),
                epoch_config: EpochConfig {
                    epoch_stake_distribution_stabilization: NonZero::new(
                        EPOCH_STAKE_DISTRIBUTION_STABILIZATION,
                    )
                    .unwrap(),
                    epoch_period_nonce_buffer: NonZero::new(EPOCH_PERIOD_NONCE_BUFFER).unwrap(),
                    epoch_period_nonce_stabilization: NonZero::new(
                        EPOCH_PERIOD_NONCE_STABILIZATION,
                    )
                    .unwrap(),
                },
                sdp_config: v1::cryptarchia::SdpConfig {
                    service_params: [ServiceParameters {
                        inactivity_period: SDP_INACTIVITY_PERIOD.try_into().unwrap(),
                        epoch: SDP_EPOCH,
                    }],
                    min_stake: lb_core::sdp::MinStake {
                        threshold: MIN_STAKE_THRESHOLD,
                        timestamp: MIN_STAKE_TIMESTAMP,
                    },
                },
                learning_rate: LEARNING_RATE.try_into().expect("1 > 0"),
                pow_config: v1::cryptarchia::PoWConfig {
                    blend: v1::cryptarchia::BlendPoWConfig {
                        base_difficulty: ModulusShift::new::<BLEND_POW_BASE_DIFFICULTY_EXPONENT>(),
                        target_transactions_per_block: NonZero::new(BLEND_POW_TARGET_TXS_PER_BLOCK)
                            .unwrap(),
                        max_step: NonZero::new(BLEND_POW_MAX_STEP).unwrap(),
                        damping_num: NonZero::new(BLEND_POW_DAMPING_NUM).unwrap(),
                        damping_den_offset: BLEND_POW_DAMPING_DEN_OFFSET,
                    },
                    reward: v1::cryptarchia::RewardPoWConfig {
                        minimum_difficulty: ModulusShift::new::<
                            REWARD_POW_MINIMUM_DIFFICULTY_EXPONENT,
                        >(),
                        ema_smoothing_factor: REWARD_POW_EMA_SMOOTHING_FACTOR,
                        ema_smoothing_precision: NonZero::new(REWARD_POW_EMA_SMOOTHING_PRECISION)
                            .unwrap(),
                        target_claims_per_block: REWARD_POW_TARGET_CLAIMS_PER_BLOCK,
                        rate_num: REWARD_POW_RATE_NUM,
                        rate_den: NonZero::new(REWARD_POW_RATE_DEN).unwrap(),
                        target_claim_per_block: NonZero::new(REWARD_POW_TARGET_CLAIM_PER_BLOCK)
                            .unwrap(),
                        pow_share: REWARD_POW_SHARE,
                        share_den: NonZero::new(REWARD_POW_SHARE_DEN).unwrap(),
                        slot_window: NonZero::new(REWARD_POW_SLOT_WINDOW).unwrap(),
                    },
                },
                execution_market: v1::cryptarchia::ExecutionMarketConfig {
                    max_gas_per_block: EXECUTION_MARKET_MAX_GAS_PER_BLOCK,
                    fee_adjustment_rate: EXECUTION_MARKET_FEE_ADJUSTMENT_RATE,
                    ema_smoothing_factor: EXECUTION_MARKET_EMA_SMOOTHING_FACTOR,
                },
                storage_market: v1::cryptarchia::StorageMarketConfig {
                    max_adjustment_factor: STORAGE_MARKET_MAX_ADJUSTMENT_FACTOR,
                    ema_smoothing_factor: STORAGE_MARKET_EMA_SMOOTHING_FACTOR,
                },
                block_rewards: BLOCK_REWARDS,
                gas_profile: GAS_PROFILE,
            },
            time: TimeDeploymentSettings {
                slot_duration_in_seconds: NonZero::new(slot_duration_in_secs)
                    .expect("The slot duration cannot be zero seconds."),
            },
        })),
        GenesisBlock::genesis(genesis_tx.clone()),
    )
    .expect("The e2e deployment must be valid.")
}
