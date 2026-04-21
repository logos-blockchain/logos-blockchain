use std::{env as std_env, error::Error, time::Duration};

use lb_testing_framework::{
    CoreBuilderExt as _, DeploymentBuilder, LbcLocalDeployer, ScenarioBuilder,
    ScenarioBuilderExt as _, TopologyConfig, configs::network::NetworkLayout, env,
    run_with_failure_diagnostics,
};
use testing_framework_core::scenario::Deployer as _;

const RUN_DURATION_SECS: u64 = 60 * 60;
const NODE_COUNT: usize = 3;

/// Long-running cluster monitor that records LIB and tip divergence across all
/// nodes.
///
/// Runtime and cluster size can be tuned via `LOGOS_BLEND_MONITOR_*`
/// environment variables.
#[tokio::test]
#[ignore = "long-running fork detector; tune with LOGOS_BLEND_MONITOR_* env vars"]
async fn cluster_fork_detector() -> Result<(), Box<dyn Error + Send + Sync>> {
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

    let deployment_builder = DeploymentBuilder::new(TopologyConfig::empty())
        .nodes(node_count)
        .with_network_layout(NetworkLayout::Full)
        .scenario_base_dir(std_env::temp_dir());

    let mut scenario = ScenarioBuilder::new(Box::new(deployment_builder))
        .with_block_feed()
        .with_run_duration(run_duration)
        .expect_cluster_fork_monitor()
        .build()?;

    let deployer = LbcLocalDeployer::default();
    let runner = deployer.deploy(&scenario).await?;
    let _handle = run_with_failure_diagnostics(runner, &mut scenario).await?;

    Ok(())
}
