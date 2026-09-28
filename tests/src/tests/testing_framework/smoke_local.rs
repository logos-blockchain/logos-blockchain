use std::time::Duration;

use lb_testing_framework::{
    AppHostDeployer, ScenarioBuilder, ScenarioBuilderExt as _, run_with_failure_diagnostics,
};

#[tokio::test]
async fn smoke_two_validators_run_180s() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // `LOGOS_BLOCKCHAIN_NODE_BIN` can point to a prebuilt node binary; otherwise
    // the testing framework builds one before starting local nodes.
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let duration = Duration::from_mins(3);
    let mut scenario =
        ScenarioBuilder::deployment_with(|t| t.nodes(1).scenario_base_dir(std::env::temp_dir()))
            .with_run_duration(duration)
            .expect_consensus_liveness()
            .build()?;
    let runner = AppHostDeployer.deploy(&scenario).await?;
    let _handle = run_with_failure_diagnostics(runner, &mut scenario).await?;
    Ok(())
}
