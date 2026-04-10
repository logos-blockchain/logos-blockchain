mod cucumber_smoke_k8s;

use std::path::PathBuf;

use cucumber::World as _;
use logos_blockchain_tests::cucumber::{
    defaults::{
        ARTEFACTS, SCENARIO_OUTPUT_DIR_REL, init_logging_defaults, init_node_log_dir_defaults,
        init_tracing,
    },
    world::{CucumberWorld, DeployerKind},
};

#[tokio::test]
async fn cucumber_local_idle_smoke() {
    // Required env vars (set on the command line when running this test):
    // - `LOGOS_BLOCKCHAIN_NODE_BIN=...`
    // - `RUST_LOG=info` (optional; better visibility)

    init_logging_defaults();
    init_node_log_dir_defaults(
        &DeployerKind::Local,
        Some(&PathBuf::from(SCENARIO_OUTPUT_DIR_REL).join(ARTEFACTS)),
    );
    init_tracing();

    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let feature_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("cucumber_tests/features/local_idle_smoke.feature");

    CucumberWorld::cucumber().run_and_exit(feature_path).await;
}
