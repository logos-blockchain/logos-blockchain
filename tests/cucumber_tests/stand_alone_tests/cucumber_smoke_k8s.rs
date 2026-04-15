use std::path::PathBuf;

use cucumber::World as _;
use lb_testing_framework::is_truthy_env;
use logos_blockchain_tests::cucumber::{
    defaults::{
        ARTEFACTS, SCENARIO_OUTPUT_DIR_REL, init_logging_defaults, init_node_log_dir_defaults,
        init_tracing,
    },
    world::{CucumberWorld, DeployerKind},
};

#[tokio::test]
async fn cucumber_k8s_idle_smoke() {
    if !is_truthy_env("CUCUMBER_RUN_K8S") {
        eprintln!("Skipping k8s cucumber smoke test; set CUCUMBER_RUN_K8S=1 to enable it.");

        return;
    }

    init_logging_defaults();
    init_node_log_dir_defaults(
        &DeployerKind::K8s,
        Some(&PathBuf::from(SCENARIO_OUTPUT_DIR_REL).join(ARTEFACTS)),
    );
    init_tracing();

    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let feature_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("cucumber_tests/stand_alone_tests/features/k8s_idle_smoke.feature");

    CucumberWorld::cucumber().run_and_exit(feature_path).await;
}
