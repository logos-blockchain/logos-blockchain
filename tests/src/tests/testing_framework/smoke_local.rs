use std::time::Duration;

use testing_framework_core::scenario::{Deployer as _, ScenarioBuilder};
use testing_framework_runner_local::LocalDeployer;
use testing_framework_workflows::ScenarioBuilderExt as _;

#[tokio::test]
async fn smoke_two_validators_run_30s() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Required env vars (set on the command line when running this test):
    // - `POL_PROOF_DEV_MODE=true` (required for local proof generation)
    // - `NOMOS_NODE_BIN=...` (path to `nomos-node` binary)
    // - `NOMOS_EXECUTOR_BIN=...` (optional; only needed if the scenario spawns
    //   executors)
    // - `RUST_LOG=info` (optional; better visibility)
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let duration = Duration::from_secs(30);

    let mut scenario =
        ScenarioBuilder::topology_with(|t| t.network_star().validators(2).executors(0))
            .with_run_duration(duration)
            .expect_consensus_liveness()
            .build()?;

    let deployer = LocalDeployer::default().with_membership_check(false);
    let runner = deployer.deploy(&scenario).await?;

    let _handle = runner.run(&mut scenario).await?;
    Ok(())
}
