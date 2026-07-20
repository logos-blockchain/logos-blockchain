use std::{
    fs,
    path::{Component, Path},
};

use lb_testing_framework::NodeStateSnapshotStore;

use crate::cucumber::{
    defaults::snapshots_root_dir,
    error::{StepError, StepResult},
    world::NodeSnapshot,
};

fn snapshot_store() -> NodeStateSnapshotStore {
    NodeStateSnapshotStore::new(snapshots_root_dir())
}

pub(super) fn reset_named_snapshot(snapshot_name: &str) -> StepResult {
    validate_snapshot_path_component(snapshot_name, "Snapshot name")?;

    let snapshot_dir = snapshots_root_dir().join(snapshot_name);
    if snapshot_dir.exists() {
        fs::remove_dir_all(&snapshot_dir).map_err(|source| StepError::LogicalError {
            message: format!(
                "failed to reset snapshot directory '{}': {source}",
                snapshot_dir.display()
            ),
        })?;
    }

    Ok(())
}

pub(super) fn validate_snapshot_path_component(
    value: &str,
    field_name: &str,
) -> Result<(), StepError> {
    if value.trim().is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{field_name} cannot be empty"),
        });
    }

    let path = Path::new(value);
    let mut components = path.components();

    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !path.is_absolute() => Ok(()),
        _ => Err(StepError::InvalidArgument {
            message: format!("{field_name} must be a single safe path component, got `{value}`"),
        }),
    }
}

/// Saves the current node state into a named snapshot location.
pub fn save_named_node_state_snapshot(
    snapshot_name: &str,
    node_name: &str,
    node_runtime_dir: &Path,
) -> StepResult {
    snapshot_store()
        .save_node(snapshot_name, node_name, node_runtime_dir)
        .map(|_| ())
        .map_err(|source| StepError::LogicalError {
            message: source.to_string(),
        })
}

/// Replaces a runtime node state with the contents from a snapshot node
/// directory.
pub fn restore_node_state_from_snapshot(
    node_snapshot: &NodeSnapshot,
    runtime_node_dir: &Path,
) -> StepResult {
    validate_snapshot_path_component(&node_snapshot.name, "Snapshot name")?;
    validate_snapshot_path_component(&node_snapshot.node, "Node name")?;

    snapshot_store()
        .restore_named_node(&node_snapshot.name, &node_snapshot.node, runtime_node_dir)
        .map_err(|source| StepError::LogicalError {
            message: format!(
                "failed to restore node state snapshot {}/{} into '{}': {source}",
                node_snapshot.name,
                node_snapshot.node,
                runtime_node_dir.display()
            ),
        })
}
