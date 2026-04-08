use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use lb_libp2p::{PeerId, identity, identity::ed25519};
use lb_node::UserConfig;
use lb_testing_framework::{CoreBuilderExt as _, ScenarioBuilder};
use tokio::time::{Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::cucumber::{
    TARGET,
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

pub fn resolve_literal_or_env(value: &str, field_name: &str) -> Result<String, StepError> {
    let trimmed = value.trim();
    if let Some(raw_name) = trimmed
        .strip_prefix("env(")
        .and_then(|v| v.strip_suffix(')'))
    {
        let var_name = raw_name.trim();
        if var_name.is_empty() {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "invalid {field_name}: expected env(VAR_NAME) with a non-empty variable name"
                ),
            });
        }

        return env::var(var_name).map_err(|e| {
            let detail = match e {
                env::VarError::NotPresent => "is not set".to_owned(),
                env::VarError::NotUnicode(_) => "is not valid unicode".to_owned(),
            };
            StepError::InvalidArgument {
                message: format!(
                    "invalid {field_name}: environment variable `{var_name}` {detail}"
                ),
            }
        });
    }

    Ok(trimmed.to_owned())
}

pub fn parse_deployer(value: &str) -> Result<DeployerKind, StepError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "local" | "host" => Ok(DeployerKind::Local),
        "compose" | "docker" => Ok(DeployerKind::Compose),
        "k8s" | "kubernetes" => Ok(DeployerKind::K8s),
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

#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn track_progress<Fut>(operation: &str, interval: Duration, wait: Fut) -> StepResult
where
    Fut: Future<Output = StepResult>,
{
    info!(target: TARGET, "Waiting for {operation}");

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
                        target: TARGET,
                        "{operation} failed after {:.2?}: {source}",
                        started_at.elapsed()
                    );
                })?;
                break;
            }
            _ = progress.tick() => {
                info!(
                    target: TARGET,
                    "Still waiting for {operation} after {:.2?}",
                    started_at.elapsed()
                );
            }
        }
    }

    info!(
        target: TARGET,
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

/// Reads a node YAML user config file and extracts the `PeerId` from the node
/// key.
pub fn peer_id_from_node_yaml(path: &Path) -> Result<PeerId, StepError> {
    let config = user_config_from_node_yaml(path)?;

    let node_key = config.network.backend.swarm.node_key;

    let keypair = identity::Keypair::from(ed25519::Keypair::from(node_key));

    Ok(PeerId::from(keypair.public()))
}

fn user_config_from_node_yaml(path: &Path) -> Result<UserConfig, StepError> {
    let config: UserConfig = {
        let text = fs::read_to_string(path).map_err(|e| StepError::LogicalError {
            message: format!("Failed to read '{}': {e}", path.display()),
        })?;

        serde_yaml::from_str(&text).map_err(|e| StepError::LogicalError {
            message: format!("Failed to parse '{}': {e}", path.display()),
        })?
    };

    Ok(config)
}

/// Reads a node YAML user config file and extracts the funding wallet public
/// key. Returns the key from `wallet.known_keys` that is not the
/// `voucher_master_key_id`.
pub fn funding_wallet_pk_from_node_yaml(path: &Path) -> Result<String, StepError> {
    let config = user_config_from_node_yaml(path)?;

    config
        .wallet
        .known_keys
        .keys()
        .find(|&key| key != &config.wallet.voucher_master_key_id)
        .cloned()
        .ok_or_else(|| StepError::LogicalError {
            message: format!(
                "No wallet public key found in 'wallet.known_keys' (other than voucher_master_key_id) in '{}'",
                path.display()
            ),
        })
}

/// Extracts the child directory name that starts with a known prefix,
/// considering the ignore list.
pub fn extract_child_dir_name(
    base_dir: &Path,
    prefix: &str,
    ignore_list: &[String],
) -> Result<String, StepError> {
    let entries = base_dir.read_dir().map_err(|e| StepError::LogicalError {
        message: format!("No child dir entries in '{}': {e}", base_dir.display()),
    })?;

    let mut matching_dirs = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(prefix) && !ignore_list.contains(&name) {
            matching_dirs.push(name);
        }
    }

    matching_dirs.sort_unstable(); // ← DETERMINISTIC ORDERING

    match matching_dirs.len() {
        1 => Ok(matching_dirs.into_iter().next().unwrap()),
        0 => Err(StepError::LogicalError {
            message: format!("No directory found starting with {prefix}"),
        }),
        _ => Err(StepError::LogicalError {
            message: format!("Ambiguous: multiple dirs match {prefix}: {matching_dirs:?}",),
        }),
    }
}

/// Returns a list of child directory names that start with a known prefix.
#[must_use]
pub fn matching_child_dirs(partial_persist_dir: &Path, prefix: &str) -> Vec<String> {
    let base_dir = partial_persist_dir.parent().unwrap_or(partial_persist_dir);

    let mut dirs = fs::read_dir(base_dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    name.starts_with(prefix).then_some(name)
                })
                .collect::<Vec<_>>()
        },
    );
    dirs.sort_unstable();
    dirs
}

/// Truncate hash for display purposes
#[must_use]
pub fn truncate_hash(input: &str, length: usize) -> String {
    input.chars().take(length).collect()
}

/// Recursively copies a directory tree from `src` into `dst`.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.exists() {
        return Err(std::io::Error::other(format!(
            "Directory '{}' is missing",
            src.display()
        )));
    }
    if !src.is_dir() {
        return Err(std::io::Error::other(format!(
            "Item '{}' is not a directory",
            src.display()
        )));
    }
    fs::create_dir_all(dst)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = dst.join(entry.file_name());
        let metadata = entry.metadata()?;

        if metadata.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

/// Returns a string representation of the last levels components of the given
/// path joined by '/'.
#[must_use]
pub fn display_last_path_components(path: &Path, levels: usize) -> String {
    let components = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let start = components.len().saturating_sub(levels);
    components[start..].join("/")
}
