use std::{
    env as std_env,
    error::Error,
    num::{NonZero, NonZeroU64},
    time::Duration,
};

use lb_node::config::{RunConfig, network::serde::nat::Config as NatConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcLocalDeployer, ScenarioBuilder, ScenarioBuilderExt as _, TopologyConfig,
    configs, env,
};
use testing_framework_core::scenario::Deployer as _;

const RUN_DURATION_SECS: u64 = 60 * 60;
const NODE_COUNT: usize = 3;

const BLEND_SLOT_SECS: u64 = 1;
const BLEND_ROUNDS_PER_SESSION: u64 = 120;
const BLEND_ROUNDS_PER_INTERVAL: u64 = 30;
const BLEND_ROUNDS_PER_OBSERVATION_WINDOW: u64 = 300;
const BLEND_ROUNDS_PER_SESSION_TRANSITION_PERIOD: u64 = 10;
const BLEND_EPOCH_TRANSITION_PERIOD_IN_SLOTS: u64 = 60;
const BLEND_MAX_RELEASE_DELAY_ROUNDS: u64 = 1;
const BLEND_NUM_LAYERS: u64 = 3;
const BLEND_MAX_DIAL_ATTEMPTS_PER_PEER: u64 = 10;
const SECURITY_PARAM: u32 = 10;

fn nz_u64(value: u64, field: &str) -> NonZeroU64 {
    NonZeroU64::new(value).unwrap_or_else(|| panic!("{field} must be > 0"))
}

/// Applies blend- and consensus-related overrides tuned for fork monitoring.
fn apply_overrides(run_config: &mut RunConfig, node_count: usize) {
    run_config.deployment.time.slot_duration = Duration::from_secs(BLEND_SLOT_SECS);

    let timing = &mut run_config.deployment.blend.common.timing;
    timing.rounds_per_interval = nz_u64(BLEND_ROUNDS_PER_INTERVAL, "rounds_per_interval");

    timing.rounds_per_observation_window = nz_u64(
        BLEND_ROUNDS_PER_OBSERVATION_WINDOW,
        "rounds_per_observation_window",
    );

    timing.rounds_per_session = nz_u64(BLEND_ROUNDS_PER_SESSION, "rounds_per_session");

    timing.rounds_per_session_transition_period = nz_u64(
        BLEND_ROUNDS_PER_SESSION_TRANSITION_PERIOD,
        "rounds_per_session_transition_period",
    );

    timing.epoch_transition_period_in_slots = nz_u64(
        BLEND_EPOCH_TRANSITION_PERIOD_IN_SLOTS,
        "epoch_transition_period_in_slots",
    );

    run_config
        .deployment
        .blend
        .core
        .scheduler
        .delayer
        .maximum_release_delay_in_rounds =
        nz_u64(BLEND_MAX_RELEASE_DELAY_ROUNDS, "max_release_delay");

    run_config.deployment.blend.common.num_blend_layers =
        nz_u64(BLEND_NUM_LAYERS, "num_blend_layers");

    run_config.deployment.cryptarchia.security_param =
        NonZero::new(SECURITY_PARAM).expect("security_param must be > 0");

    let max_peers = u64::try_from(node_count.saturating_sub(1))
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1);

    run_config.user.blend.core.backend.core_peering_degree = 1..=max_peers;
    run_config
        .user
        .blend
        .core
        .backend
        .max_dial_attempts_per_peer = nz_u64(BLEND_MAX_DIAL_ATTEMPTS_PER_PEER, "max dial attempts");

    let swarm_port = run_config.user.network.backend.swarm.port;
    let external_addr = format!("/ip4/127.0.0.1/udp/{swarm_port}/quic-v1")
        .parse()
        .expect("static NAT external address should be valid multiaddr");

    run_config.user.network.backend.swarm.nat = NatConfig::Static {
        external_address: external_addr,
    };
}

/// Long-running cluster monitor that records LIB and tip divergence across all
/// nodes.
///
/// Runtime and cluster size can be tuned via `LOGOS_BLEND_MONITOR_*`
/// environment variables.
#[tokio::test]
#[ignore = "long-running fork detector; tune with LOGOS_BLEND_MONITOR_* env vars"]
async fn cluster_fork_detector_three_nodes() -> Result<(), Box<dyn Error + Send + Sync>> {
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let run_duration = Duration::from_secs(env::env_u64(
        "LOGOS_BLEND_MONITOR_RUN_SECS",
        RUN_DURATION_SECS,
    ));
    let node_count = env::env_opt::<usize>("LOGOS_BLEND_MONITOR_NODE_COUNT")
        .filter(|value| *value > 0)
        .unwrap_or(NODE_COUNT);

    let mut deployment_builder = DeploymentBuilder::new(TopologyConfig::empty())
        .nodes(node_count)
        .with_network_layout(configs::network::NetworkLayout::Full)
        .scenario_base_dir(std_env::temp_dir());
    let deployment = deployment_builder.clone().build()?;

    for node in &deployment.plans {
        let mut run_config = configs::build_node_run_config(&deployment, node, None)?;
        apply_overrides(&mut run_config, node_count);
        deployment_builder = deployment_builder.with_node_config_override(node.index, run_config);
    }

    let mut scenario = ScenarioBuilder::new(Box::new(deployment_builder))
        .with_run_duration(run_duration)
        .expect_cluster_fork_monitor()
        .build()?;

    let deployer = LbcLocalDeployer::default();
    let runner = deployer.deploy(&scenario).await?;
    let _handle = runner.run(&mut scenario).await?;

    Ok(())
}
