//! Saved test inputs and native Logos configuration, independent of binary
//! selection.

use std::{
    collections::HashSet,
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use lb_testing_framework::SharedDeployment;
use libp2p::identity::{Keypair, ed25519};
use serde::Deserialize;
use serde_yaml::Value;
use testing_framework_core::scenario::DynError;

/// Index of native files and the test metadata that cannot be read from the
/// current checkout's configuration types. Paths are relative to this file.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedConfigBundle {
    deployment: PathBuf,
    nodes: Vec<PathBuf>,
    // Passed to the other implementation together with the native deployment.
    slots_per_epoch: NonZeroU64,
}

/// Saved network inputs and native Logos files for the mixed test.
#[derive(Clone)]
pub struct SavedDeployment {
    shared: SharedDeployment,
    pub(crate) nodes: Vec<SavedNode>,
}

#[derive(Clone)]
pub struct SavedNode {
    pub(crate) path: PathBuf,
    pub(crate) yaml: String,
}

impl SavedDeployment {
    /// Loads files and identities without selecting or executing a binary.
    /// A scenario can use a prefix of the saved node configurations.
    pub fn load(path: &Path, capacity: usize) -> Result<Self, DynError> {
        let bundle: PreparedConfigBundle = serde_yaml::from_str(&read_config(path)?)
            .with_context(|| format!("invalid configuration bundle {}", path.display()))?;
        if capacity == 0 || capacity > bundle.nodes.len() {
            return Err(format!(
                "prepared configuration has {} nodes, but the scenario requires {capacity}",
                bundle.nodes.len()
            )
            .into());
        }

        let root = path.parent().unwrap_or_else(|| Path::new("."));
        let deployment_yaml = read_config(&root.join(bundle.deployment))?;
        let mut nodes = Vec::new();
        let mut peer_ids = HashSet::new();
        let mut network_keys = Vec::new();

        for config in bundle.nodes.iter().take(capacity) {
            let config = root.join(config);
            let yaml = read_config(&config)?;
            let key = read_network_key(&yaml)
                .with_context(|| format!("invalid network identity in {}", config.display()))?;
            let peer_id = key.public().to_peer_id();
            if !peer_ids.insert(peer_id) {
                return Err(format!("duplicate peer ID {peer_id} in {}", config.display()).into());
            }
            network_keys.push(key);
            nodes.push(SavedNode { path: config, yaml });
        }

        Ok(Self {
            shared: SharedDeployment::from_parts(
                Some(deployment_yaml),
                network_keys,
                Some(bundle.slots_per_epoch),
            ),
            nodes,
        })
    }

    /// The same adapter inputs used for generated deployments, independent of
    /// the selected binary and its native node configuration files.
    #[must_use]
    pub const fn shared_deployment(&self) -> &SharedDeployment {
        &self.shared
    }
}

fn read_config(path: &Path) -> anyhow::Result<String> {
    fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

// Read only the identity field shared by the supported Logos versions. The
// selected binary verifies its interpretation; the rest of its schema stays
// opaque.
fn read_network_key(yaml: &str) -> anyhow::Result<Keypair> {
    let config: Value = serde_yaml::from_str(yaml)?;
    let encoded = config["network"]["backend"]["swarm"]["node_key"]
        .as_str()
        .context("network.backend.swarm.node_key must be a hex-encoded Ed25519 key")?;
    let secret = ed25519::SecretKey::try_from_bytes(hex::decode(encoded)?)?;
    Ok(Keypair::from(ed25519::Keypair::from(secret)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{SavedDeployment, read_network_key};

    #[test]
    fn saved_shared_inputs_preserve_native_settings_and_identities() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/logos-0.3.0-rc.5");
        let saved = SavedDeployment::load(&root.join("cluster.yaml"), 2).unwrap();
        let shared = saved.shared_deployment();

        assert_eq!(shared.node_count(), 2);
        assert_eq!(
            shared.deployment_yaml().unwrap(),
            std::fs::read_to_string(root.join("deployment.yaml")).unwrap(),
        );
        for (index, node) in saved.nodes.iter().enumerate() {
            let original = std::fs::read_to_string(&node.path).unwrap();
            assert_eq!(node.yaml, original);
            assert_eq!(
                shared.network_key(index).unwrap().public(),
                read_network_key(&original).unwrap().public(),
            );
        }
        assert!(shared.network_key(2).is_err());
    }
}
