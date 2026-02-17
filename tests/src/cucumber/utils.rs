use std::{env, path::PathBuf, time::Duration};

use lb_testing_framework::{CoreBuilderExt as _, ScenarioBuilder};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::cucumber::{
    error::{StepError, StepResult},
    world::{DeployerKind, NetworkKind, TopologySpec},
};

type ScenarioBuilderWith = ScenarioBuilder;

#[must_use]
pub fn make_builder(topology: &TopologySpec) -> ScenarioBuilderWith {
    ScenarioBuilder::deployment_with(|t| {
        let base = match topology.network {
            NetworkKind::Star => t,
        };
        base.nodes(topology.nodes.get())
            .scenario_base_dir(topology.scenario_base_dir.clone())
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

pub async fn track_progress<Fut>(operation: &str, interval: Duration, wait: Fut) -> StepResult
where
    Fut: Future<Output = StepResult>,
{
    info!(target: super::TARGET, "Waiting for {operation}");

    let started_at = Instant::now();

    let mut wait_task = Box::pin(wait);
    let mut progress = tokio::time::interval(interval);

    progress.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let _ = progress.tick().await;

    loop {
        tokio::select! {
            result = &mut wait_task => {
                result.inspect_err(|source| {
                    warn!(
                        target: super::TARGET,
                        "{operation} failed after {:.2?}: {source}",
                        started_at.elapsed()
                    );
                })?;
                break;
            }
            _ = progress.tick() => {
                info!(
                    target: super::TARGET,
                    "Still waiting for {operation} after {:.2?}",
                    started_at.elapsed()
                );
            }
        }
    }

    info!(
        target: super::TARGET,
        "{operation} completed in {:.2?}",
        started_at.elapsed()
    );

    Ok(())
}

#[macro_export]
macro_rules! non_zero {
    ($field:expr, $value:expr) => {
        std::num::NonZero::new($value).ok_or_else(|| StepError::InvalidArgument {
            message: format!("'{}' must be > 0", $field),
        })
    };
}
