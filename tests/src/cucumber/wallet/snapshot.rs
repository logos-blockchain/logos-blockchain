use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use hex::FromHex as _;
use lb_core::header::HeaderId;
use lb_testing_framework::configs::wallet::WalletAccount;
use serde::{Deserialize, Serialize};
use testing_framework_core::scenario::{DynError, SnapshotArtifact, SnapshotStore};
use tokio::time::sleep;
use tracing::{debug, info};

use crate::{
    common::wallet::{TrackedWalletsState, WalletUtxos},
    cucumber::{
        defaults::snapshots_root_dir,
        error::{StepError, StepResult},
        wallet::TARGET,
        world::{CucumberWorld, WalletInfoMap},
    },
};

/// Snapshot extension id used for Cucumber wallet state.
pub const WALLET_SNAPSHOT_EXTENSION_ID: &str = "wallet";
const WALLET_SNAPSHOT_FEED_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WALLET_SNAPSHOT_FEED_WAIT_INTERVAL: Duration = Duration::from_millis(250);

/// Serializable Cucumber wallet state.
///
/// This is test-framework state, not node state. It contains wallet aliases,
/// account keys, and wallet UTXOs observed at the saved node recovery tip so
/// wallet checks can continue from the snapshot point without scanning from
/// genesis again.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletSnapshot {
    wallet_info: WalletInfoMap,
    wallet_accounts: HashMap<usize, WalletAccount>,
    states_by_node: HashMap<String, WalletNodeSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WalletNodeSnapshot {
    tip: String,
    height: u64,
    tracked_wallets: TrackedWalletsState,
}

#[derive(Deserialize)]
struct ChainServiceRecoveryState {
    tip: String,
}

impl WalletSnapshot {
    /// Build a wallet snapshot from live nodes.
    ///
    /// Each node entry is captured at that node's current consensus tip. The
    /// wallet feed must already have observed the same tip, otherwise the
    /// snapshot would pair node state with stale wallet state.
    async fn from_active_world_for_nodes(
        world: &CucumberWorld,
        node_names: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, StepError> {
        let wallet_info = world.wallet_info.clone();
        let wallet_accounts = world.wallet_accounts.clone();
        if wallet_info.is_empty() && wallet_accounts.is_empty() {
            return Ok(Self {
                wallet_info,
                wallet_accounts,
                states_by_node: HashMap::new(),
            });
        }

        let mut states_by_node = HashMap::new();
        for node_name in node_names {
            let node_name = node_name.as_ref();
            let node = world
                .nodes_info
                .get(node_name)
                .ok_or_else(|| StepError::LogicalError {
                    message: format!("wallet snapshot source node `{node_name}` is not running"),
                })?;

            let tip = node
                .started_node
                .client
                .consensus_info()
                .await?
                .cryptarchia_info
                .tip;

            let node_snapshot = wait_for_wallet_feed_state_at_tip(world, node_name, &tip).await?;

            states_by_node.insert(node_name.to_owned(), node_snapshot);
        }

        Ok(Self {
            wallet_info,
            wallet_accounts,
            states_by_node,
        })
    }

    /// Build a wallet snapshot for node state already saved on disk.
    ///
    /// This is used after node snapshots are copied. The recovery file in each
    /// saved node snapshot is treated as the source of truth for the node tip,
    /// and wallet state is taken only once the feed has that exact tip.
    async fn from_saved_node_tips(
        world: &CucumberWorld,
        snapshot_name: &str,
        node_names: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<Self, StepError> {
        let wallet_info = world.wallet_info.clone();
        let wallet_accounts = world.wallet_accounts.clone();
        if wallet_info.is_empty() && wallet_accounts.is_empty() {
            return Ok(Self {
                wallet_info,
                wallet_accounts,
                states_by_node: HashMap::new(),
            });
        }

        let mut states_by_node = HashMap::new();
        for node_name in node_names {
            let node_name = node_name.as_ref();
            let saved_tip = saved_node_tip(snapshot_name, node_name)?;
            let saved_tip = parse_header_id(&saved_tip)?;
            let node_snapshot = wait_for_wallet_feed_state_at_tip(world, node_name, &saved_tip)
                .await
                .map_err(|error| StepError::LogicalError {
                    message: format!(
                        "cannot save wallet snapshot `{snapshot_name}` for node `{node_name}`: \
                         {error}"
                    ),
                })?;

            states_by_node.insert(node_name.to_owned(), node_snapshot);
        }

        Ok(Self {
            wallet_info,
            wallet_accounts,
            states_by_node,
        })
    }

    fn is_empty(&self) -> bool {
        self.wallet_info.is_empty()
            && self.wallet_accounts.is_empty()
            && self.states_by_node.is_empty()
    }

    fn validate_saved_node_tips(&self, snapshot_name: &str) -> StepResult {
        for (node_name, node_snapshot) in &self.states_by_node {
            let saved_tip = saved_node_tip(snapshot_name, node_name)?;
            let saved_tip = normalize_header_id(&saved_tip);
            let wallet_tip = normalize_header_id(&node_snapshot.tip);

            if saved_tip != wallet_tip {
                return Err(StepError::LogicalError {
                    message: format!(
                        "cannot save wallet snapshot `{snapshot_name}`: node `{node_name}` saved \
                         tip `{saved_tip}` does not match prepared wallet tip `{wallet_tip}`"
                    ),
                });
            }
        }

        Ok(())
    }

    fn apply_metadata(&self, world: &mut CucumberWorld) {
        world.wallet_info.clone_from(&self.wallet_info);
        world.wallet_accounts.clone_from(&self.wallet_accounts);
    }

    fn apply_for_node(&self, node_name: &str, world: &CucumberWorld) -> StepResult {
        let Some(node_snapshot) = self.states_by_node.get(node_name) else {
            return Err(StepError::LogicalError {
                message: format!("wallet snapshot does not contain state for node `{node_name}`"),
            });
        };

        let wallet_keys = world.wallet_tracking_keys_for_source(node_name)?;
        let source_wallet_ids = wallet_keys
            .iter()
            .map(|keys| keys.wallet_id().clone())
            .collect::<HashSet<_>>();
        let source_wallet_utxos = node_snapshot
            .tracked_wallets
            .to_wallet_utxos()
            .into_iter()
            .filter(|(wallet_id, _)| source_wallet_ids.contains(wallet_id))
            .collect::<WalletUtxos>();

        world.with_wallets_mut(|wallets| {
            wallets.replace_current_wallets_utxos(source_wallet_utxos.clone());
        })?;

        if !wallet_keys.is_empty() {
            let tip = parse_header_id(&node_snapshot.tip)?;
            world
                .with_wallet_feed_state_mut(|tracker, _wallets| {
                    tracker.replace_source_state(
                        node_name.to_owned(),
                        &wallet_keys,
                        source_wallet_utxos,
                        tip,
                        node_snapshot.height,
                    )
                })?
                .map_err(|error| StepError::LogicalError {
                    message: format!("failed to restore wallet feed state: {error}"),
                })?;
        }

        Ok(())
    }

    fn into_artifact(self) -> Result<SnapshotArtifact, DynError> {
        let wallet_count = self.wallet_info.len();
        let account_count = self.wallet_accounts.len();
        let node_count = self.states_by_node.len();

        Ok(SnapshotArtifact::new(
            2,
            serde_json::json!({
                "wallet_count": wallet_count,
                "account_count": account_count,
                "node_count": node_count,
            }),
            serde_json::to_value(self)?,
        ))
    }

    fn from_artifact(artifact: &SnapshotArtifact) -> Result<Self, DynError> {
        Ok(serde_json::from_value(artifact.payload.clone())?)
    }
}

/// Prepare Cucumber wallet state from the active node tips before node
/// shutdown.
///
/// Use this for snapshot-on-stop flows. It captures wallet state while nodes
/// are still queryable, then `save_prepared_wallet_snapshot` validates that the
/// saved node snapshot still points at the same tips before writing the wallet
/// artifact.
pub async fn prepare_wallet_snapshot_from_active_node_tips(
    world: &mut CucumberWorld,
) -> StepResult {
    world.ensure_wallet_block_feed().await?;
    let node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();
    let snapshot = WalletSnapshot::from_active_world_for_nodes(world, node_names).await?;
    world.snapshot_save_config.prepared_wallet_snapshot = Some(snapshot);

    Ok(())
}

/// Save Cucumber wallet state after the feed observes the saved node snapshot
/// tips from active nodes.
///
/// Use this when node state was saved while nodes are still running. The saved
/// node recovery tips are read from disk, then wallet state is selected from
/// the feed at those exact tips.
pub async fn save_wallet_snapshot_from_saved_node_tips(
    snapshot_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    world.ensure_wallet_block_feed().await?;
    let node_names = world.nodes_info.keys().cloned().collect::<Vec<_>>();

    save_wallet_snapshot_for_saved_nodes(snapshot_name, world, node_names).await
}

/// Save the wallet snapshot prepared before node shutdown.
///
/// This is the final half of snapshot-on-stop. It fails if no wallet snapshot
/// was prepared, or if the node recovery tips saved on disk no longer match the
/// prepared wallet state.
pub fn save_prepared_wallet_snapshot(snapshot_name: &str, world: &mut CucumberWorld) -> StepResult {
    let Some(snapshot) = world.snapshot_save_config.prepared_wallet_snapshot.take() else {
        return Err(StepError::LogicalError {
            message: format!("wallet snapshot `{snapshot_name}` was not prepared before shutdown"),
        });
    };

    snapshot.validate_saved_node_tips(snapshot_name)?;
    save_wallet_snapshot_value(snapshot_name, snapshot)
}

/// Save Cucumber wallet state after the feed observes the saved node snapshot
/// tips from active nodes.
///
/// This is the node-selected variant used by manual-control snapshot commands.
/// It writes no wallet artifact when the scenario has no wallet resources.
pub async fn save_wallet_snapshot_for_saved_nodes(
    snapshot_name: &str,
    world: &mut CucumberWorld,
    node_names: impl IntoIterator<Item = impl AsRef<str>>,
) -> StepResult {
    world.ensure_wallet_block_feed().await?;
    let snapshot = WalletSnapshot::from_saved_node_tips(world, snapshot_name, node_names).await?;
    save_wallet_snapshot_value(snapshot_name, snapshot)
}

fn save_wallet_snapshot_value(snapshot_name: &str, snapshot: WalletSnapshot) -> StepResult {
    if snapshot.is_empty() {
        return Ok(());
    }

    let artifact = snapshot.into_artifact().map_err(|e| snapshot_error(&e))?;

    SnapshotStore::new(snapshots_root_dir())
        .save_extension_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID, artifact)
        .map(|_| ())
        .map_err(|e| snapshot_error(&e))
}

/// Prepare wallet state restoration from `snapshot_name`.
///
/// Missing wallet state is allowed here because generic snapshot restore is
/// extension-aware but not extension-specific. A malformed wallet artifact
/// still fails the step.
///
/// This restores wallet metadata before nodes are started: named wallets,
/// account keys, and empty runtime tracking structures. Node-specific UTXO
/// state is applied later by `restore_wallet_snapshot_if_present`, once a node
/// is actually being started from the snapshot.
pub fn prepare_wallet_snapshot_restore_if_present(
    snapshot_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    let Some(snapshot) = read_wallet_snapshot_if_present(snapshot_name)? else {
        return Ok(());
    };

    clear_wallet_snapshot_state(world)?;
    snapshot.apply_metadata(world);
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

/// Restore any wallet state stored in `snapshot_name`.
///
/// Missing wallet state is allowed here because generic snapshot restore is
/// extension-aware but not extension-specific. A malformed wallet artifact
/// still fails the step.
///
/// `node_name` is the source-node entry inside the snapshot, not necessarily
/// the runtime node being started. This supports the common restore shape where
/// several fresh nodes all start from one saved node snapshot.
pub fn restore_wallet_snapshot_if_present(
    snapshot_name: &str,
    node_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    let Some(snapshot) = read_wallet_snapshot_if_present(snapshot_name)? else {
        return Ok(());
    };

    snapshot.apply_for_node(node_name, world)?;
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn read_wallet_snapshot_if_present(
    snapshot_name: &str,
) -> Result<Option<WalletSnapshot>, StepError> {
    let artifact = SnapshotStore::new(snapshots_root_dir())
        .read_manifest(snapshot_name)
        .map_err(|e| snapshot_error(&e))?
        .extensions
        .get(WALLET_SNAPSHOT_EXTENSION_ID)
        .cloned();

    artifact
        .map(|artifact| WalletSnapshot::from_artifact(&artifact).map_err(|e| snapshot_error(&e)))
        .transpose()
}

async fn wait_for_wallet_feed_state_at_tip(
    world: &CucumberWorld,
    node_name: &str,
    tip: &HeaderId,
) -> Result<WalletNodeSnapshot, StepError> {
    let started_at = Instant::now();
    let tip = tip.to_string();

    info!(
        target: TARGET,
        "Waiting for wallet feed state at snapshot tip `{tip}` for node `{node_name}`"
    );

    loop {
        if let Some((tip, height, tracked_wallets)) = world
            .with_wallets(|wallets| wallets.export_state_for_node_at_header(node_name, &tip))?
        {
            info!(
                target: TARGET,
                "Wallet feed state reached snapshot tip `{tip}` for node `{node_name}` at height {height}"
            );

            return Ok(WalletNodeSnapshot {
                tip,
                height,
                tracked_wallets,
            });
        }

        if started_at.elapsed() >= WALLET_SNAPSHOT_FEED_WAIT_TIMEOUT {
            return Err(StepError::Timeout {
                message: format!(
                    "wallet feed did not observe snapshot tip `{tip}` for node `{node_name}` \
                     within {} seconds",
                    WALLET_SNAPSHOT_FEED_WAIT_TIMEOUT.as_secs()
                ),
            });
        }

        debug!(
            target: TARGET,
            "Still waiting for wallet feed state at snapshot tip `{tip}` for node `{node_name}`"
        );
        sleep(WALLET_SNAPSHOT_FEED_WAIT_INTERVAL).await;
    }
}

fn clear_wallet_snapshot_state(world: &mut CucumberWorld) -> StepResult {
    world.wallet_info.clear();
    world.wallet_accounts.clear();
    world.fee_state.clear_reservations();
    world.with_wallets_mut(|wallets| {
        wallets.replace_from_state(TrackedWalletsState::default());
    })?;

    world.reset_wallet_block_feed();
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn saved_node_tip(snapshot_name: &str, node_name: &str) -> Result<String, StepError> {
    let path = saved_node_recovery_path(snapshot_name, node_name);
    let recovery = fs::read_to_string(&path).map_err(|source| StepError::LogicalError {
        message: format!(
            "failed to read saved node recovery state '{}': {source}",
            path.display()
        ),
    })?;

    let state: ChainServiceRecoveryState =
        serde_json::from_str(&recovery).map_err(|source| StepError::LogicalError {
            message: format!(
                "failed to parse saved node recovery state '{}': {source}",
                path.display()
            ),
        })?;

    Ok(state.tip)
}

fn saved_node_recovery_path(snapshot_name: &str, node_name: &str) -> PathBuf {
    snapshots_root_dir()
        .join(snapshot_name)
        .join(node_name)
        .join("recovery")
        .join("consensus")
        .join("chain_service.json")
}

fn normalize_header_id(value: &str) -> String {
    value.strip_prefix("0x").unwrap_or(value).to_owned()
}

fn parse_header_id(value: &str) -> Result<HeaderId, StepError> {
    let value_without_prefix = value.strip_prefix("0x").unwrap_or(value);
    <[u8; 32]>::from_hex(value_without_prefix)
        .map(HeaderId::from)
        .map_err(|source| StepError::LogicalError {
            message: format!("invalid wallet snapshot header id `{value}`: {source}"),
        })
}

fn snapshot_error(source: &DynError) -> StepError {
    StepError::LogicalError {
        message: source.to_string(),
    }
}
