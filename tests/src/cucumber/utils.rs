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
        base.nodes(topology.validators.get())
    })
}

#[must_use]
pub fn is_truthy_env(key: &str) -> bool {
    env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
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

#[macro_export]
macro_rules! non_zero {
    ($field:expr, $value:expr) => {
        std::num::NonZero::new($value).ok_or_else(|| StepError::InvalidArgument {
            message: format!("'{}' must be > 0", $field),
        })
    };
}
