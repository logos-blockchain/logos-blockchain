use std::path::{Path, PathBuf};

use lb_node::config::storage::serde::RocksDbSettings;
use testing_framework_core::scenario::{DynError, NodeStateSource, SnapshotStore};

/// The DB folder name the harness configures its nodes with, which a node
/// suffixes with the version of the state it persists.
pub const NODE_DB_FOLDER: &str = "db";

/// The subdirectory of a node runtime directory its DB is in.
#[must_use]
pub fn node_db_subdir() -> String {
    RocksDbSettings {
        folder_name: NODE_DB_FOLDER.to_owned(),
        ..RocksDbSettings::default()
    }
    .db_path(Path::new(""))
    .to_string_lossy()
    .into_owned()
}

/// Snapshot store for Logos node local state.
///
/// Logos node state is represented by the DB subdirectory under a node
/// runtime directory. This wrapper fixes that application-specific state shape
/// while delegating the generic snapshot layout to [`SnapshotStore`].
#[derive(Clone, Debug)]
pub struct NodeStateSnapshotStore {
    store: SnapshotStore,
}

impl NodeStateSnapshotStore {
    /// Create a node-state snapshot store rooted at `root_dir`.
    #[must_use]
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            store: SnapshotStore::new(root_dir),
        }
    }

    /// Save one node's local state into a named snapshot.
    ///
    /// The saved source is keyed by `node_name` in the snapshot manifest and
    /// stored under `<root>/<snapshot_name>/<node_name>/`.
    pub fn save_node(
        &self,
        snapshot_name: &str,
        node_name: &str,
        runtime_dir: &Path,
    ) -> Result<String, DynError> {
        self.store
            .save_node_dirs(snapshot_name, node_name, runtime_dir, &[&node_db_subdir()])
    }

    /// Restore node state from `source` into an existing runtime directory.
    ///
    /// This clears and replaces only the DB subdirectory.
    pub fn restore_node(
        &self,
        source: &NodeStateSource,
        runtime_dir: &Path,
    ) -> Result<(), DynError> {
        self.store
            .restore_node_dirs(source, runtime_dir, &[&node_db_subdir()])
    }

    /// Restore one named node from a named snapshot into an existing runtime
    /// dir.
    pub fn restore_named_node(
        &self,
        snapshot_name: &str,
        node_name: &str,
        runtime_dir: &Path,
    ) -> Result<(), DynError> {
        self.restore_node(
            &NodeStateSource::snapshot_node(snapshot_name, node_name),
            runtime_dir,
        )
    }
}
