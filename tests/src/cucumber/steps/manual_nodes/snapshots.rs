use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use crate::cucumber::{
    defaults::{SNAPSHOT_STATE_SUBDIRS, snapshots_root_dir},
    error::{StepError, StepResult},
    utils::copy_dir_recursive,
    world::NodeSnapshot,
};

fn snapshot_dir(snapshot_name: &str) -> PathBuf {
    snapshots_root_dir().join(snapshot_name)
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

/// Saves the current node blockchain state into a named snapshot location.
pub fn save_named_blockchain_snapshot(
    snapshot_name: &str,
    node_name: &str,
    node_runtime_dir: &Path,
) -> StepResult {
    validate_snapshot_path_component(snapshot_name, "Snapshot name")?;
    validate_snapshot_path_component(node_name, "Node name")?;

    let destination = snapshot_dir(snapshot_name).join(node_name);
    sanitize_path_and_clear_dir(&destination)?;

    for dir_name in SNAPSHOT_STATE_SUBDIRS {
        copy_dir_recursive(
            &node_runtime_dir.join(dir_name),
            &destination.join(dir_name),
        )
        .map_err(|e| StepError::LogicalError {
            message: format!(
                "failed to copy node `{node_name}` data from '{}' to '{}': {e}",
                node_runtime_dir.display(),
                destination.display()
            ),
        })?;
    }

    Ok(())
}

/// Replaces a runtime node state with the contents from a snapshot node
/// directory.
pub fn restore_node_state_from_snapshot(
    node_snapshot: &NodeSnapshot,
    runtime_node_dir: &Path,
) -> StepResult {
    validate_snapshot_path_component(&node_snapshot.name, "Snapshot name")?;
    validate_snapshot_path_component(&node_snapshot.node, "Node name")?;

    let snapshot_node_dir = snapshot_dir(&node_snapshot.name).join(&node_snapshot.node);
    for dir_name in SNAPSHOT_STATE_SUBDIRS {
        let snapshot_state_dir = snapshot_node_dir.join(dir_name);
        sanitize_path_as_dir(&snapshot_state_dir)?;
        let runtime_state_dir = runtime_node_dir.join(dir_name);
        sanitize_path_and_clear_dir(&runtime_state_dir)?;
        copy_dir_recursive(&snapshot_state_dir, &runtime_state_dir)?;
    }

    Ok(())
}

fn sanitize_path_and_clear_dir(path: &Path) -> StepResult {
    if path.exists() {
        if !path.is_dir() {
            return Err(StepError::LogicalError {
                message: format!(
                    "Failed to remove directory '{}': path exists but is not a directory",
                    path.display()
                ),
            });
        }
        fs::remove_dir_all(path).map_err(|e| StepError::LogicalError {
            message: format!(
                "Failed to clear existing directory '{}': {e}",
                path.display()
            ),
        })?;
    }
    Ok(())
}

fn sanitize_path_as_dir(path: &Path) -> StepResult {
    if !path.exists() {
        return Err(StepError::LogicalError {
            message: format!("Path does '{}' not exist", path.display(),),
        });
    }
    if !path.is_dir() {
        return Err(StepError::LogicalError {
            message: format!(
                "snapshot state path '{}' exists but is not a directory",
                path.display()
            ),
        });
    }

    Ok(())
}
