use std::time::Duration;

use lb_testing_framework::{
    CoreBuilderExt as _, LbcLocalDeployer, ScenarioBuilder, ScenarioBuilderExt as _,
};
use testing_framework_core::scenario::Deployer as _;

#[tokio::test]
async fn smoke_two_validators_run_180s() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Required env vars (set on the command line when running this test):
    // - `LOGOS_BLOCKCHAIN_NODE_BIN=...` (path to `logos-blockchain-node` binary)
    // - `RUST_LOG=info` (optional; better visibility)
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let duration = Duration::from_mins(3);
    let mut scenario =
        ScenarioBuilder::deployment_with(|t| t.nodes(2).scenario_base_dir(std::env::temp_dir()))
            .with_run_duration(duration)
            .expect_consensus_liveness()
            .build()?;
    let deployer = LbcLocalDeployer::default();
    let runner = deployer.deploy(&scenario).await?;
    let _handle = runner.run(&mut scenario).await?;
    Ok(())
}
