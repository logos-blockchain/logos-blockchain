use super::*;

/// This struct represents the wallet resources to be associated with a node at
/// startup.
pub struct WalletStartInfo {
    // Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    // The account index in the genesis tokens that this resource corresponds to.
    pub account_index: usize,
}

/// Saves the current node state of all nodes into a named snapshot location for
/// later use.
pub fn create_snapshots_all_nodes(
    world: &CucumberWorld,
    snapshot_name: &str,
) -> Result<(), StepError> {
    validate_snapshot_path_component(snapshot_name, "Snapshot name")?;
    reset_named_snapshot(snapshot_name)?;

    let runtime_dir_by_node_name: Vec<(String, PathBuf)> = world
        .nodes_info
        .iter()
        .map(|(node_name, info)| (node_name.clone(), info.runtime_dir.clone()))
        .collect();

    for (node_name, node_runtime_dir) in &runtime_dir_by_node_name {
        save_named_node_state_snapshot(snapshot_name, node_name, node_runtime_dir)?;
        info!(
            target: TARGET,
            "Saved snapshot `{snapshot_name}` for node `{node_name}`",
        );
    }
    Ok(())
}

pub async fn create_snapshot_all_nodes_with_wallet_state(
    world: &mut CucumberWorld,
    snapshot_name: &str,
) -> StepResult {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    create_and_save_all_wallets_snapshot(snapshot_name, world).await?;
    create_snapshots_all_nodes(world, snapshot_name)
}

pub async fn create_snapshot_node_with_wallet_state(
    world: &mut CucumberWorld,
    snapshot_name: &str,
    node_name: &str,
) -> StepResult {
    if world.nodes_info.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "cannot create snapshot: no running nodes".to_owned(),
        });
    }

    if let Some(runtime_dir) = world
        .nodes_info
        .get(node_name)
        .map(|info| info.runtime_dir.clone())
    {
        reset_named_snapshot(snapshot_name)?;
        create_and_save_all_wallets_snapshot(snapshot_name, world).await?;
        save_named_node_state_snapshot(snapshot_name, node_name, &runtime_dir)?;
        info!(
            target: TARGET,
            "Saved snapshot `{snapshot_name}` for node {}",
            runtime_dir.display()
        );
        Ok(())
    } else {
        Err(StepError::InvalidArgument {
            message: format!("Node {node_name} does not exist"),
        })
    }
}

/// Fetches and logs the consensus info of all nodes, for debugging purposes.
/// Does not require the nodes to be aligned or have any specific state, and is
/// resilient to some nodes being offline or unresponsive.
#[expect(
    clippy::cognitive_complexity,
    reason = "Singular fn with multiple branches to handle different events and futures."
)]
pub async fn get_cryptarchia_info_all_nodes(world: &CucumberWorld, step: &str) {
    let mut node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();
    node_names.sort();

    if node_names.is_empty() {
        warn!(
            target: TARGET,
            "Step `{step}` no nodes found for CRYPTARCHIA_INFO_ALL_NODES"
        );
        return;
    }

    for node_name in node_names {
        let Some(node_info) = world.nodes_info.get(&node_name) else {
            continue;
        };
        match node_info.started_node.client.consensus_info().await {
            Ok(consensus) => {
                let mode = if matches!(consensus.phase, PhaseTag::Following) {
                    "Online"
                } else {
                    "Bootstrapping"
                };
                info!(
                    target: TARGET,
                    "cryptarchia/info - '{}', '{}', {}/{}, tip '{} ...', lib '{} ...'",
                    node_name,
                    mode,
                    consensus.cryptarchia_info.height,
                    consensus.cryptarchia_info.slot.into_inner(),
                    truncate_hash(&consensus.cryptarchia_info.tip.encode_hex::<String>(), 16),
                    truncate_hash(&consensus.cryptarchia_info.lib.encode_hex::<String>(), 16),
                );
            }
            Err(e) => {
                warn!(
                    target: TARGET,
                    "Step `{step}` CRYPTARCHIA_INFO failed for node `{node_name}`: {e}",
                );
            }
        }
    }
}
