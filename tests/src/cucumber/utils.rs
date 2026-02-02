use std::{env, path::PathBuf};

use testing_framework_core::scenario::{Builder, ScenarioBuilder};

use crate::cucumber::{
    error::StepError,
    world::{DeployerKind, NetworkKind, TopologySpec},
};

#[must_use]
pub fn make_builder(topology: TopologySpec) -> Builder<()> {
    ScenarioBuilder::topology_with(|t| {
        let base = match topology.network {
            NetworkKind::Star => t.network_star(),
        };
        base.nodes(topology.validators)
    })
}

#[must_use]
pub fn is_truthy_env(key: &str) -> bool {
    env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

pub fn positive_usize(label: &str, value: usize) -> Result<usize, StepError> {
    if value == 0 {
        Err(StepError::InvalidArgument {
            message: format!("{label} must be > 0"),
        })
    } else {
        Ok(value)
    }
}

pub fn positive_u64(label: &str, value: u64) -> Result<u64, StepError> {
    if value == 0 {
        Err(StepError::InvalidArgument {
            message: format!("{label} must be > 0"),
        })
    } else {
        Ok(value)
    }
}

pub fn parse_deployer(value: &str) -> Result<DeployerKind, StepError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "local" | "host" => Ok(DeployerKind::Local),
        "compose" | "docker" => Ok(DeployerKind::Compose),
        other => Err(StepError::UnsupportedDeployer {
            value: other.to_owned(),
        }),
    }
}

#[must_use]
pub fn shared_host_bin_path(binary_name: &str) -> PathBuf {
    let cucumber_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cucumber_dir.join("../assets/stack/bin").join(binary_name)
}
