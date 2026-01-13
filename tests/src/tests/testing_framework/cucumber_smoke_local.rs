use std::path::PathBuf;

use cucumber::World as _;
use cucumber_ext::TestingFrameworkWorld;

#[tokio::test]
async fn cucumber_local_idle_smoke() {
    // Required env vars (set on the command line when running this test):
    // - `POL_PROOF_DEV_MODE=true`
    // - `NOMOS_NODE_BIN=...`
    // - `NOMOS_KZGRS_PARAMS_PATH=...` (path to KZG params directory/file, e.g.
    //   `tests/kzgrs`)
    // - `NOMOS_EXECUTOR_BIN=...` (optional; only needed when the scenario uses
    //   executors)
    // - `RUST_LOG=info` (optional; better visibility)
    let _init_result = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let feature_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("features/testing_framework/local_idle_smoke.feature");

    TestingFrameworkWorld::cucumber()
        .run_and_exit(feature_path)
        .await;
}
