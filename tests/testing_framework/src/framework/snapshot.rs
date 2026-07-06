use std::path::{Path, PathBuf};

use testing_framework_core::scenario::{DynError, NodeStateSource, SnapshotStore};

const NODE_STATE_SUBDIRS: [&str; 2] = ["db", "recovery"];

/// Snapshot store for Logos node local state.
///
/// Logos node state is represented by the `db` and `recovery` subdirectories
/// under a node runtime directory. This wrapper fixes that application-specific
/// state shape while delegating the generic snapshot layout to
/// [`SnapshotStore`].
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
            .save_node_dirs(snapshot_name, node_name, runtime_dir, &NODE_STATE_SUBDIRS)
    }

    /// Resolve a node-state source to a validated startup directory.
    ///
    /// The returned directory contains the `db` and `recovery` subdirectories
    /// expected by a Logos node process.
    pub fn prepare_source_dir(&self, source: &NodeStateSource) -> Result<PathBuf, DynError> {
        self.store.prepare_node_dir(source, &NODE_STATE_SUBDIRS)
    }

    /// Restore node state from `source` into an existing runtime directory.
    ///
    /// This clears and replaces only the `db` and `recovery` subdirectories.
    pub fn restore_node(
        &self,
        source: &NodeStateSource,
        runtime_dir: &Path,
    ) -> Result<(), DynError> {
        self.store
            .restore_node_dirs(source, runtime_dir, &NODE_STATE_SUBDIRS)
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
