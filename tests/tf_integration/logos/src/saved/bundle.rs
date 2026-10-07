//! Saved test inputs and native Logos configuration, independent of binary
//! selection.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use blockchain_test_support::preparation::{SharedDeployment, wallet::WalletAccount};
use lb_core::mantle::Utxo;
use lb_libp2p::identity::{Keypair, ed25519};
use serde::Deserialize;
use serde_yaml::Value;
use testing_framework_core::scenario::DynError;

/// Index of native files and the test metadata that cannot be read from the
/// current checkout's configuration types. Paths are relative to this file.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedConfigBundle {
    deployment: PathBuf,
    nodes: Vec<PathBuf>,
    // Used by scenario assertions; must match the saved deployment.
    slots_per_epoch: NonZeroU64,
    supported_scenarios: Vec<String>,
    // Test accounts and notes exported with the native genesis, not regenerated.
    #[serde(default)]
    wallets: BTreeMap<usize, WalletAccount>,
    #[serde(default)]
    genesis_utxos: Vec<Utxo>,
}

impl PreparedConfigBundle {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bundle: Self = serde_yaml::from_str(&read_config(path)?)
            .with_context(|| format!("invalid configuration bundle {}", path.display()))?;
        let mut keys = HashSet::new();
        for (index, account) in &bundle.wallets {
            if !keys.insert(account.public_key()) {
                bail!("saved wallet account {index} repeats another account's public key");
            }
        }
        let mut outputs = HashSet::new();
        for utxo in &bundle.genesis_utxos {
            if !outputs.insert((utxo.op_id, utxo.output_index)) {
                bail!("saved genesis contains a duplicate output");
            }
        }
        Ok(bundle)
    }

    /// Requires the exact initial notes requested by a scenario. Extra accounts
    /// may exist, but extra funds in a requested account change test semantics.
    pub fn require_wallet_funding(
        &self,
        account_index: usize,
        token_count: usize,
        token_amount: u64,
    ) -> anyhow::Result<()> {
        let account = self.wallets.get(&account_index).with_context(|| {
            format!("saved configuration has no wallet account {account_index}")
        })?;
        let public_key = account.public_key();
        let amounts = self
            .genesis_utxos
            .iter()
            .filter(|utxo| utxo.note.pk == public_key)
            .map(|utxo| utxo.note.value)
            .collect::<Vec<_>>();

        if amounts.len() != token_count || amounts.iter().any(|amount| *amount != token_amount) {
            bail!(
                "saved wallet account {account_index} has genesis note values {amounts:?}, \
                 but the scenario requires {token_count} notes of {token_amount} LGO"
            );
        }
        Ok(())
    }

    /// Checks setup suitability, not whether the selected binary passes the
    /// test.
    pub fn require_scenario(&self, name: &str) -> anyhow::Result<()> {
        if !self
            .supported_scenarios
            .iter()
            .any(|supported| supported == name)
        {
            bail!(
                "saved configuration does not support scenario '{name}'; supported scenarios: {}",
                self.supported_scenarios.join(", ")
            );
        }
        Ok(())
    }
}

/// Saved network inputs, native Logos files and the test data recorded with
/// them.
#[derive(Clone)]
pub struct SavedDeployment {
    shared: SharedDeployment,
    pub(crate) nodes: Vec<SavedNode>,
    wallets: BTreeMap<usize, WalletAccount>,
    genesis_utxos: Vec<Utxo>,
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
        let bundle = PreparedConfigBundle::load(path)?;
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
            shared: SharedDeployment::from_yaml(
                deployment_yaml,
                network_keys,
                bundle.slots_per_epoch,
            ),
            nodes,
            wallets: bundle.wallets,
            genesis_utxos: bundle.genesis_utxos,
        })
    }

    /// The same adapter inputs used for generated deployments, independent of
    /// the selected binary and its native node configuration files.
    #[must_use]
    pub const fn shared_deployment(&self) -> &SharedDeployment {
        &self.shared
    }

    #[must_use]
    pub const fn wallet_accounts(&self) -> &BTreeMap<usize, WalletAccount> {
        &self.wallets
    }

    #[must_use]
    pub fn genesis_utxos(&self) -> &[Utxo] {
        &self.genesis_utxos
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

    use super::{PreparedConfigBundle, SavedDeployment, read_network_key};

    #[test]
    fn saved_shared_inputs_preserve_native_settings_and_identities() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../cucumber_tests/fixtures/core-0.3.0-rc.5");
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

    #[test]
    fn saved_bundle_limits_scenarios_to_supported_setups() {
        let bundle: PreparedConfigBundle = serde_yaml::from_str(include_str!(
            "../../../../cucumber_tests/fixtures/core-0.3.0-rc.5/cluster.yaml"
        ))
        .expect("valid bundle");

        bundle.require_scenario("One node happy path").unwrap();
        let error = bundle
            .require_scenario("Two nodes immutable blocks")
            .expect_err("scenario requires typed configuration overrides");
        assert!(error.to_string().contains("Two nodes immutable blocks"));
        assert!(error.to_string().contains("supported scenarios:"));
    }

    #[test]
    fn saved_wallet_funding_requires_exact_notes_for_requested_accounts() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../cucumber_tests/fixtures/funded-wallets-0.3.0-rc.5/cluster.yaml");
        let bundle = PreparedConfigBundle::load(&path).expect("valid funded bundle");

        bundle.require_wallet_funding(1, 2, 1000).unwrap();
        bundle.require_wallet_funding(2, 0, 0).unwrap();

        for (index, count, amount) in [(1, 1, 1000), (1, 2, 500), (2, 1, 1000), (3, 0, 0)] {
            assert!(bundle.require_wallet_funding(index, count, amount).is_err());
        }
    }
}
