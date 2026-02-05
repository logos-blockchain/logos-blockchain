use std::{fs, path::PathBuf};

use tracing_subscriber::{EnvFilter, fmt};

use crate::cucumber::world::DeployerKind;

const FEATURES_DIR_REL: &str = "cucumber_tests/features/";

const SCENARIO_OUTPUT_DIR_REL: &str = "cucumber_tests/temp";
const SCENARIO_NODE_LOG_DIR_REL: &str = "cucumber_tests/temp/node-logs";
const CONTAINER_NODE_LOG_DIR: &str = "/tmp/node-logs";

fn set_default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: Used as an early-run default. Prefer setting env vars in the
        // shell for multi-threaded runs.
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

pub fn init_logging_defaults() {
    set_default_env("NOMOS_TESTS_KEEP_LOGS", "1");
    set_default_env("NOMOS_LOG_LEVEL", "info");
    set_default_env("RUST_LOG", "info");
}

pub fn init_node_log_dir_defaults(deployer: DeployerKind) {
    if std::env::var_os("NOMOS_LOG_DIR").is_some() {
        return;
    }

    let current_dir = std::env::current_dir().expect("should exist");
    let host_dir = current_dir.join(SCENARIO_NODE_LOG_DIR_REL);
    fs::create_dir_all(&host_dir).expect("should succeed");

    match deployer {
        DeployerKind::Local => set_default_env("NOMOS_LOG_DIR", &host_dir.display().to_string()),
        DeployerKind::Compose => set_default_env("NOMOS_LOG_DIR", CONTAINER_NODE_LOG_DIR),
    }
}

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _unused = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

#[must_use]
pub fn create_scenario_output_dir() -> PathBuf {
    let current_dir = std::env::current_dir().expect("should exist");
    println!("Current directory: {}", current_dir.display());
    let output_dir = current_dir.join(SCENARIO_OUTPUT_DIR_REL);
    fs::create_dir_all(output_dir.clone()).expect("should succeed");
    println!("Output directory: {}", output_dir.display());
    output_dir
}

#[must_use]
pub fn get_feature_path() -> PathBuf {
    let feature_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FEATURES_DIR_REL);
    if matches!(fs::exists(feature_path.clone()), Ok(true)) {
        println!("Feature path:      {}", feature_path.display());
    } else {
        panic!("Feature path does not exist: {}", feature_path.display());
    }
    feature_path
}
