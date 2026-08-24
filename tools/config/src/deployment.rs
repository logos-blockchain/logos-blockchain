use core::{
    num::{NonZero, NonZeroU64},
    time::Duration,
};

use lb_core::{
    block::genesis::GenesisBlock,
    sdp::{NumberOfEpochs, ServiceType},
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::ModulusShift;
use lb_libp2p::protocol_name::StreamProtocol;
use lb_node::config::{
    blend::deployment::{
        CommonSettings as BlendCommonSettings, CoreSettings as BlendCoreSettings,
        CoverTrafficSettings, MessageDelayerSettings, MinimumNetworkSize, SchedulerSettings,
        Settings as BlendDeploymentSettings,
    },
    cryptarchia::deployment::{
        EpochConfig, ServiceParameters, Settings as CryptarchiaDeploymentSettings,
    },
    deployment::DeploymentSettings,
    mempool::deployment::{Settings as MempoolDeploymentSettings, default_tx_ttl},
    network::deployment::Settings as NetworkDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};
use lb_utils::math::{NonNegativeRatio, PositiveF64};

use crate::{
    release::ProtocolIdentity,
    time::{CONSENSUS_SLOT_TIME_VAR, DEFAULT_SLOT_TIME_IN_SECS},
};

const MINIMUM_BLEND_NETWORK_SIZE: u64 = 2;
const NUM_BLEND_LAYERS: u64 = 3;
const BLEND_PROTOCOL_NAME: &str = "/blend/integration-tests";
const DATA_REPLICATION_FACTOR: u64 = 0;

const MINIMUM_MESSAGES_COEFFICIENT: u64 = 1;
const BLEND_NORMALIZATION_CONSTANT: f64 = 1.03;
const COVER_MESSAGE_FREQUENCY_PER_ROUND: f64 = 1.0;
const MAXIMUM_RELEASE_DELAY_IN_ROUNDS: u64 = 3;
const ACTIVITY_THRESHOLD_SENSITIVITY: u64 = 1;

const IDENTIFY_PROTOCOL_SUFFIX: &str = "identify/1.0.0";
const KADEMLIA_PROTOCOL_SUFFIX: &str = "kad/1.0.0";
const CHAIN_SYNC_PROTOCOL_SUFFIX: &str = "chainsync/1.0.0";
const GOSSIPSUB_PROTOCOL_SUFFIX: &str = "cryptarchia/proto/1.0.0";

const SECURITY_PARAM: u32 = 20;
const UNCLE_REFERENCE_WINDOW_IN_BLOCK: u32 = 12;
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

const MEMPOOL_TOPIC: &str = "mantle_e2e_tests";
const DEFAULT_PROTOCOL_NAMESPACE: &str = "integration/logos-blockchain";

#[must_use]
pub fn e2e_deployment_settings_with_genesis_block(
    genesis_block: &GenesisBlock,
) -> DeploymentSettings {
    let genesis_tx = genesis_block.genesis_tx();
    let slot_duration_in_secs = std::env::var(CONSENSUS_SLOT_TIME_VAR)
        .map_or(DEFAULT_SLOT_TIME_IN_SECS, |s| s.parse::<u64>().unwrap());

    let protocol_identity = ProtocolIdentity::from_env(DEFAULT_PROTOCOL_NAMESPACE);

    DeploymentSettings {
        blend: BlendDeploymentSettings {
            common: BlendCommonSettings {
                minimum_network_size: MinimumNetworkSize::try_new(MINIMUM_BLEND_NETWORK_SIZE)
                    .expect("Minimum network size cannot be less than 2."),
                num_blend_layers: NonZeroU64::try_from(NUM_BLEND_LAYERS)
                    .expect("Number of blend layers cannot be zero."),
                protocol_name: StreamProtocol::new(BLEND_PROTOCOL_NAME),
                data_replication_factor: DATA_REPLICATION_FACTOR,
            },
            core: BlendCoreSettings {
                minimum_messages_coefficient: NonZeroU64::try_from(MINIMUM_MESSAGES_COEFFICIENT)
                    .expect("Minimum messages coefficient cannot be zero."),
                normalization_constant: BLEND_NORMALIZATION_CONSTANT
                    .try_into()
                    .expect("Normalization constant cannot be negative."),
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
                        message_frequency_per_round: PositiveF64::try_from(
                            COVER_MESSAGE_FREQUENCY_PER_ROUND,
                        )
                        .expect("Message frequency per round must be positive."),
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
        network: NetworkDeploymentSettings {
            identify_protocol_name: protocol_identity.stream_protocol(IDENTIFY_PROTOCOL_SUFFIX),
            kademlia_protocol_name: protocol_identity.stream_protocol(KADEMLIA_PROTOCOL_SUFFIX),
            chain_sync_protocol_name: protocol_identity.stream_protocol(CHAIN_SYNC_PROTOCOL_SUFFIX),
        },
        cryptarchia: CryptarchiaDeploymentSettings {
            gossipsub_protocol: protocol_identity.protocol_name(GOSSIPSUB_PROTOCOL_SUFFIX),
            security_param: NonZero::new(SECURITY_PARAM).unwrap(),
            uncle_reference_window_in_block: NonZero::new(UNCLE_REFERENCE_WINDOW_IN_BLOCK).unwrap(),
            slot_activation_coeff: NonNegativeRatio::new(
                SLOT_ACTIVATION_COEFF_NUMERATOR,
                NonZero::new(SLOT_ACTIVATION_COEFF_DENOMINATOR).unwrap(),
            ),
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(
                    EPOCH_STAKE_DISTRIBUTION_STABILIZATION,
                )
                .unwrap(),
                epoch_period_nonce_buffer: NonZero::new(EPOCH_PERIOD_NONCE_BUFFER).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(EPOCH_PERIOD_NONCE_STABILIZATION)
                    .unwrap(),
            },
            sdp_config: lb_node::config::cryptarchia::deployment::SdpConfig {
                service_params: [(
                    ServiceType::BlendNetwork,
                    ServiceParameters {
                        inactivity_period: SDP_INACTIVITY_PERIOD.try_into().unwrap(),
                        epoch: SDP_EPOCH,
                    },
                )]
                .into(),
                min_stake: lb_core::sdp::MinStake {
                    threshold: MIN_STAKE_THRESHOLD,
                    timestamp: MIN_STAKE_TIMESTAMP,
                },
            },
            genesis_block: GenesisBlock::genesis(genesis_tx),
            learning_rate: LEARNING_RATE.try_into().expect("1 > 0"),
            faucet_pk: None,
            pow_config: lb_node::config::cryptarchia::deployment::PoWConfig {
                blend: lb_node::config::cryptarchia::deployment::BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<BLEND_POW_BASE_DIFFICULTY_EXPONENT>(),
                    target_transactions_per_block: NonZero::new(BLEND_POW_TARGET_TXS_PER_BLOCK)
                        .unwrap(),
                    max_step: NonZero::new(BLEND_POW_MAX_STEP).unwrap(),
                    damping_num: NonZero::new(BLEND_POW_DAMPING_NUM).unwrap(),
                    damping_den_offset: BLEND_POW_DAMPING_DEN_OFFSET,
                },
            },
        },
        time: TimeDeploymentSettings {
            slot_duration: Duration::from_secs(slot_duration_in_secs),
        },
        mempool: MempoolDeploymentSettings {
            pubsub_topic: MEMPOOL_TOPIC.to_owned(),
            tx_ttl: default_tx_ttl(),
        },
    }
}
