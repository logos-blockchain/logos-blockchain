use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use hex::FromHex as _;
use lb_core::header::HeaderId;
use lb_testing_framework::configs::wallet::WalletAccount;
use serde::{Deserialize, Serialize};
use testing_framework_core::scenario::{DynError, SnapshotArtifact, SnapshotStore};
use tokio::time::{Instant, sleep};

use crate::{
    common::wallet::TrackedWalletsState,
    cucumber::{
        defaults::snapshots_root_dir,
        error::{StepError, StepResult},
        world::{CucumberWorld, WalletInfoMap},
    },
};

/// Snapshot extension id used for Cucumber wallet state.
pub const WALLET_SNAPSHOT_EXTENSION_ID: &str = "wallet";
const WALLET_SNAPSHOT_STATE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const WALLET_SNAPSHOT_STATE_WAIT_INTERVAL: Duration = Duration::from_millis(100);

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
    async fn from_world_for_nodes(
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
            let (tip, height, tracked_wallets) =
                wait_for_wallet_state_at_saved_tip(world, snapshot_name, node_name, &saved_tip)
                    .await?;

            states_by_node.insert(
                node_name.to_owned(),
                WalletNodeSnapshot {
                    tip,
                    height,
                    tracked_wallets,
                },
            );
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

    fn apply_for_node(self, node_name: &str, world: &mut CucumberWorld) -> StepResult {
        let Some(node_snapshot) = self.states_by_node.get(node_name) else {
            return Err(StepError::LogicalError {
                message: format!("wallet snapshot does not contain state for node `{node_name}`"),
            });
        };

        world.wallet_info = self.wallet_info;
        world.wallet_accounts = self.wallet_accounts;

        let wallet_utxos = node_snapshot.tracked_wallets.to_wallet_utxos();

        world.with_wallets_mut(|wallets| {
            wallets.replace_from_state(node_snapshot.tracked_wallets.clone());
        })?;

        let wallet_keys = world.wallet_tracking_keys_for_source(node_name)?;
        if !wallet_keys.is_empty() {
            let tip = parse_header_id(&node_snapshot.tip)?;
            world
                .with_wallet_feed_state_mut(|tracker, _wallets| {
                    tracker.replace_source_state(
                        node_name.to_owned(),
                        &wallet_keys,
                        wallet_utxos,
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

/// Save Cucumber wallet state as a wallet extension artifact in
/// `snapshot_name`.
pub async fn save_wallet_snapshot(snapshot_name: &str, world: &CucumberWorld) -> StepResult {
    let node_names = world
        .nodes_info
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();

    save_wallet_snapshot_for_nodes(snapshot_name, world, node_names).await
}

/// Save Cucumber wallet state for the saved node snapshot entries.
pub async fn save_wallet_snapshot_for_nodes(
    snapshot_name: &str,
    world: &CucumberWorld,
    node_names: impl IntoIterator<Item = impl AsRef<str>>,
) -> StepResult {
    let snapshot = WalletSnapshot::from_world_for_nodes(world, snapshot_name, node_names).await?;
    if snapshot.is_empty() {
        return Ok(());
    }

    let artifact = snapshot.into_artifact().map_err(|e| snapshot_error(&e))?;

    SnapshotStore::new(snapshots_root_dir())
        .save_extension_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID, artifact)
        .map(|_| ())
        .map_err(|e| snapshot_error(&e))
}

/// Restore any wallet state stored in `snapshot_name`.
///
/// Missing wallet state is allowed here because generic snapshot restore is
/// extension-aware but not extension-specific. A malformed wallet artifact
/// still fails the step.
pub fn restore_wallet_snapshot_if_present(
    snapshot_name: &str,
    node_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    let artifact = SnapshotStore::new(snapshots_root_dir())
        .read_manifest(snapshot_name)
        .map_err(|e| snapshot_error(&e))?
        .extensions
        .get(WALLET_SNAPSHOT_EXTENSION_ID)
        .cloned();

    let Some(artifact) = artifact else {
        return Ok(());
    };

    let snapshot = WalletSnapshot::from_artifact(&artifact).map_err(|e| snapshot_error(&e))?;
    clear_wallet_snapshot_state(world)?;

    snapshot.apply_for_node(node_name, world)?;
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn clear_wallet_snapshot_state(world: &mut CucumberWorld) -> StepResult {
    world.wallet_info.clear();
    world.wallet_accounts.clear();
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

async fn wait_for_wallet_state_at_saved_tip(
    world: &CucumberWorld,
    snapshot_name: &str,
    node_name: &str,
    saved_tip: &str,
) -> Result<(String, u64, TrackedWalletsState), StepError> {
    let started_at = Instant::now();

    loop {
        if let Some(state) = world
            .with_wallets(|wallets| wallets.export_state_for_node_at_header(node_name, saved_tip))?
        {
            return Ok(state);
        }

        if started_at.elapsed() >= WALLET_SNAPSHOT_STATE_WAIT_TIMEOUT {
            return Err(StepError::LogicalError {
                message: format!(
                    "cannot save wallet snapshot `{snapshot_name}`: wallet state for node \
                    `{node_name}` saved tip `{saved_tip}` is not available after {} seconds",
                    WALLET_SNAPSHOT_STATE_WAIT_TIMEOUT.as_secs()
                ),
            });
        }

        sleep(WALLET_SNAPSHOT_STATE_WAIT_INTERVAL).await;
    }
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
