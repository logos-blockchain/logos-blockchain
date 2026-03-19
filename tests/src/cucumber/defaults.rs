use std::{fs, path::PathBuf};

use lb_testing_framework::LOGOS_BLOCKCHAIN_LOG_LEVEL;
use tracing::warn;
use tracing_subscriber::{EnvFilter, fmt};

use crate::cucumber::world::DeployerKind;

const FEATURES_DIR_REL: &str = "cucumber_tests/features/";

pub const SCENARIO_OUTPUT_DIR_REL: &str = "cucumber_tests/temp";
pub const ARTEFACTS: &str = "cucumber_artefacts";
const CONTAINER_NODE_LOG_DIR: &str = "/tmp/node_logs";

const TARGET: &str = "cucumber_defaults";

const LOGOS_BLOCKCHAIN_TESTS_TRACING: &str = "LOGOS_BLOCKCHAIN_TESTS_TRACING";
const TF_KEEP_LOGS: &str = "TF_KEEP_LOGS";
const CUCUMBER_LOG_LEVEL: &str = "CUCUMBER_LOG_LEVEL";
const RUST_LOG: &str = "RUST_LOG";
const LOGOS_BLOCKCHAIN_LOG_DIR: &str = "LOGOS_BLOCKCHAIN_LOG_DIR";
const CUCUMBER_RETRIES: &str = "CUCUMBER_RETRIES";
pub const LOGOS_BLOCKCHAIN_NODE_BIN: &str = "LOGOS_BLOCKCHAIN_NODE_BIN";

/// Set an environment variable to a default value if it is not already set.
pub fn set_default_env(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: Used as an early-run default. Prefer setting env vars in the
        // shell for multi-threaded runs.
        unsafe {
            std::env::set_var(key, value);
        }
    }
}

pub fn init_logging_defaults() {
    set_default_env(LOGOS_BLOCKCHAIN_TESTS_TRACING, "false");
    set_default_env(TF_KEEP_LOGS, "true");
    // Always keep RUST_LOG at info for console output
    set_default_env(RUST_LOG, "info");

    std::env::var_os(CUCUMBER_LOG_LEVEL).map_or_else(
        || {
            set_default_env(LOGOS_BLOCKCHAIN_LOG_LEVEL, "info");
        },
        |log_level| {
            let log_level = log_level.to_string_lossy().to_lowercase();
            match log_level.as_str() {
                "trace" | "debug" | "info" | "warn" | "error" => {
                    set_default_env(LOGOS_BLOCKCHAIN_LOG_LEVEL, log_level.as_str());
                }
                other => {
                    warn!(
                        target: TARGET,
                        "Invalid log level '{other}' in {CUCUMBER_LOG_LEVEL}; using 'info' level"
                    );
                    set_default_env(LOGOS_BLOCKCHAIN_LOG_LEVEL, "info");
                }
            }
        },
    );
}

pub fn init_node_log_dir_defaults(deployer: &DeployerKind, log_dir: Option<&PathBuf>) {
    let host_dir = match deployer {
        DeployerKind::Local => log_dir.as_ref().map_or_else(
            || {
                std::env::var_os(LOGOS_BLOCKCHAIN_LOG_DIR).map_or_else(
                    || {
                        let dir = PathBuf::from(SCENARIO_OUTPUT_DIR_REL).join(ARTEFACTS);
                        set_default_env(LOGOS_BLOCKCHAIN_LOG_DIR, &dir.display().to_string());
                        dir
                    },
                    |dir| {
                        let dir = dir.to_string_lossy().as_ref().to_owned();
                        PathBuf::from(dir)
                    },
                )
            },
            |dir| {
                // set_default_env(LOGOS_BLOCKCHAIN_LOG_DIR, &dir.display().to_string());
                PathBuf::from(dir)
            },
        ),
        DeployerKind::Compose => std::env::var_os(LOGOS_BLOCKCHAIN_LOG_DIR).map_or_else(
            || {
                set_default_env(LOGOS_BLOCKCHAIN_LOG_DIR, CONTAINER_NODE_LOG_DIR);
                PathBuf::from(CONTAINER_NODE_LOG_DIR)
            },
            |dir| {
                let dir = dir.to_string_lossy().as_ref().to_owned();
                PathBuf::from(dir)
            },
        ),
    };
    fs::create_dir_all(&host_dir).expect("should succeed");
}

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _unused = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Get the number of retries for failed scenarios from the `CUCUMBER_RETRIES`
/// environment variable. If the variable is not set, defaults to 2 retries. If
/// the variable is set to 0, returns None.
pub fn get_retries() -> Result<Option<usize>, String> {
    std::env::var_os(CUCUMBER_RETRIES).map_or_else(
        || Ok(Some(2)),
        |retries| {
            retries
                .to_string_lossy()
                .as_ref()
                .to_owned()
                .parse()
                .map_or_else(
                    |_| {
                        Err(format!(
                            "Invalid value for {CUCUMBER_RETRIES}: '{}'",
                            retries.to_string_lossy()
                        ))
                    },
                    |retries| {
                        if retries == 0 {
                            Ok(None)
                        } else {
                            Ok(Some(retries))
                        }
                    },
                )
        },
    )
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
