use core::{
    num::{NonZero, NonZeroU64},
    time::Duration,
};
use std::sync::OnceLock;

use lb_core::{mantle::genesis_tx::GenesisTx, sdp::ServiceType};
use lb_libp2p::protocol_name::StreamProtocol;
use lb_node::config::{
    blend::deployment::{
        CommonSettings as BlendCommonSettings, CoreSettings as BlendCoreSettings,
        CoverTrafficSettings, MessageDelayerSettings, SchedulerSettings,
        Settings as BlendDeploymentSettings, TimingSettings,
    },
    cryptarchia::deployment::{
        EpochConfig, ServiceParameters, Settings as CryptarchiaDeploymentSettings,
    },
    deployment::DeploymentSettings,
    mempool::deployment::Settings as MempoolDeploymentSettings,
    network::deployment::Settings as NetworkDeploymentSettings,
    time::deployment::Settings as TimeDeploymentSettings,
};
use lb_utils::math::NonNegativeF64;
use time::OffsetDateTime;

use crate::topology::configs::time::{CONSENSUS_SLOT_TIME_VAR, DEFAULT_SLOT_TIME_IN_SECS};

static CHAIN_START_TIME: OnceLock<OffsetDateTime> = OnceLock::new();

fn get_or_init_chain_start_time() -> OffsetDateTime {
    *CHAIN_START_TIME.get_or_init(OffsetDateTime::now_utc)
}

#[must_use]
pub fn e2e_deployment_settings_with_genesis_tx(genesis_tx: GenesisTx) -> DeploymentSettings {
    let slot_duration_in_secs = std::env::var(CONSENSUS_SLOT_TIME_VAR)
        .map(|s| s.parse::<u64>().unwrap())
        .unwrap_or(DEFAULT_SLOT_TIME_IN_SECS);

    DeploymentSettings {
        blend: BlendDeploymentSettings {
            common: BlendCommonSettings {
                minimum_network_size: NonZeroU64::try_from(1u64)
                    .expect("Minimum network size cannot be zero."),
                num_blend_layers: NonZeroU64::try_from(3)
                    .expect("Number of blend layers cannot be zero."),
                timing: TimingSettings {
                    round_duration: Duration::from_secs(1),
                    rounds_per_interval: NonZeroU64::try_from(30u64)
                        .expect("Rounds per interval cannot be zero."),
                    // (21,600 blocks * 30s per block) / 1s per round = 648,000 rounds
                    rounds_per_session: NonZeroU64::try_from(648_000u64)
                        .expect("Rounds per session cannot be zero."),
                    rounds_per_observation_window: NonZeroU64::try_from(30u64)
                        .expect("Rounds per observation window cannot be zero."),
                    rounds_per_session_transition_period: NonZeroU64::try_from(30u64)
                        .expect("Rounds per session transition period cannot be zero."),
                    epoch_transition_period_in_slots: NonZeroU64::try_from(2_600)
                        .expect("Epoch transition period in slots cannot be zero."),
                },
                protocol_name: StreamProtocol::new("/blend/integration-tests"),
                data_replication_factor: 0,
            },
            core: BlendCoreSettings {
                minimum_messages_coefficient: NonZeroU64::try_from(1)
                    .expect("Minimum messages coefficient cannot be zero."),
                normalization_constant: 1.03f64
                    .try_into()
                    .expect("Normalization constant cannot be negative."),
                scheduler: SchedulerSettings {
                    cover: CoverTrafficSettings {
                        intervals_for_safety_buffer: 100,
                        message_frequency_per_round: NonNegativeF64::try_from(1f64)
                            .expect("Message frequency per round cannot be negative."),
                    },
                    delayer: MessageDelayerSettings {
                        maximum_release_delay_in_rounds: NonZeroU64::try_from(1u64)
                            .expect("Maximum release delay between rounds cannot be zero."),
                    },
                },
                activity_threshold_sensitivity: 1,
            },
        },
        network: NetworkDeploymentSettings {
            identify_protocol_name: StreamProtocol::new(
                "/integration/logos-blockchain/identify/1.0.0",
            ),
            kademlia_protocol_name: StreamProtocol::new("/integration/logos-blockchain/kad/1.0.0"),
            chain_sync_protocol_name: StreamProtocol::new(
                "/integration/logos-blockchain/chainsync/1.0.0",
            ),
        },
        cryptarchia: CryptarchiaDeploymentSettings {
            gossipsub_protocol: "/integration/logos-blockchain/cryptarchia/proto/1.0.0".to_owned(),
            // by setting the slot coeff to 1, we also increase the probability of multiple
            // blocks (forks) being produced in the same slot (epoch).
            // Setting the security parameter to some value > 1 ensures
            // nodes have some time to sync before deciding on the
            // longest chain.
            security_param: NonZero::new(10).unwrap(),
            epoch_config: EpochConfig {
                epoch_stake_distribution_stabilization: NonZero::new(3).unwrap(),
                epoch_period_nonce_buffer: NonZero::new(3).unwrap(),
                epoch_period_nonce_stabilization: NonZero::new(4).unwrap(),
            },
            sdp_config: lb_node::config::cryptarchia::deployment::SdpConfig {
                service_params: [(
                    ServiceType::BlendNetwork,
                    ServiceParameters {
                        lock_period: 10,
                        inactivity_period: 1,
                        retention_period: 1,
                        timestamp: 0,
                    },
                )]
                .into(),
                min_stake: lb_core::sdp::MinStake {
                    threshold: 1,
                    timestamp: 0,
                },
            },
            genesis_state: genesis_tx,
        },
        time: TimeDeploymentSettings {
            slot_duration: Duration::from_secs(slot_duration_in_secs),
            chain_start_time: get_or_init_chain_start_time(),
        },
        mempool: MempoolDeploymentSettings {
            pubsub_topic: "mantle_e2e_tests".to_owned(),
        },
    }
}
