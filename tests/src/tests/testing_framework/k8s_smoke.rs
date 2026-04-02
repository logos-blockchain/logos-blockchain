use std::{error::Error, thread, time::Duration};

use lb_testing_framework::{
    CoreBuilderExt as _, K8sRunnerError, LbcK8sDeployer, ScenarioBuilder, ScenarioBuilderExt as _,
};
use testing_framework_core::scenario::Deployer as _;

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

#[test]
#[ignore = "requires kubectl, helm, and a reachable Kubernetes cluster"]
fn smoke_two_validators_run_on_k8s() -> TestResult {
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    run_k8s_smoke_in_thread()
}

fn run_k8s_smoke_in_thread() -> TestResult {
    thread::spawn(run_k8s_smoke)
        .join()
        .map_err(|panic| -> Box<dyn Error + Send + Sync> {
            std::io::Error::other(format_panic(&panic)).into()
        })?
}

fn run_k8s_smoke() -> TestResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let run_handle = runtime.block_on(async {
        let mut scenario = ScenarioBuilder::deployment_with(|topology| {
            topology.nodes(2).scenario_base_dir(std::env::temp_dir())
        })
        .with_run_duration(Duration::from_secs(180))
        .expect_consensus_liveness()
        .build()
        .map_err(|err| -> Box<dyn Error + Send + Sync> { err.into() })?;

        let deployer = LbcK8sDeployer::default();
        let runner = match deployer.deploy(&scenario).await {
            Ok(runner) => runner,
            Err(K8sRunnerError::ClientInit { source }) => {
                tracing::warn!("Kubernetes cluster unavailable ({source}); skipping");

                return Ok(None);
            }
            Err(err) => return Err(Box::<dyn Error + Send + Sync>::from(err)),
        };

        let handle = runner
            .run(&mut scenario)
            .await
            .map_err(|err| -> Box<dyn Error + Send + Sync> { err.into() })?;

        Ok(Some(handle))
    })?;

    if let Some(run_handle) = run_handle {
        drop(run_handle);
    }

    Ok(())
}

fn format_panic(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        return format!("k8s smoke thread panicked: {message}");
    }

    if let Some(message) = panic.downcast_ref::<String>() {
        return format!("k8s smoke thread panicked: {message}");
    }

    "k8s smoke thread panicked".to_owned()
}
