use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use hex::FromHex as _;
use lb_core::header::HeaderId;
use lb_testing_framework::{NodeHttpClient, configs::wallet::WalletAccount};
use serde::{Deserialize, Serialize};
use testing_framework_core::scenario::{DynError, SnapshotArtifact, SnapshotStore};

use crate::{
    common::wallet::{
        TrackedWallets, TrackedWalletsState, WalletId, WalletUtxos,
        scanner::config::{DEFAULT_SCANNER_SNAPSHOT_RESCAN_BLOCKS, ScannerSeed},
    },
    cucumber::{
        defaults::snapshots_root_dir,
        error::{StepError, StepResult},
        world::{CucumberWorld, WalletInfoMap},
    },
};

/// Snapshot extension id used for Cucumber wallet state.
pub const WALLET_SNAPSHOT_EXTENSION_ID: &str = "wallet";

/// Serializable Cucumber wallet state.
///
/// This is test-framework state, not node state. It contains wallet aliases,
/// account keys, and wallet UTXOs observed at scanner-applied tips so
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
    #[serde(default)]
    slot: Option<u64>,
    tracked_wallets: TrackedWalletsState,
}

impl WalletSnapshot {
    /// Build a wallet snapshot from the scanner-applied group tips.
    ///
    /// Scanner catch-up has already selected and applied the canonical group
    /// tips, so this avoids racing against fresh per-node consensus queries
    /// while nodes are still running.
    fn from_scanner_state(world: &CucumberWorld) -> Result<Self, StepError> {
        let wallet_info = world.wallet_info.clone();
        let wallet_accounts = world.wallet_accounts.clone();
        if wallet_info.is_empty() && wallet_accounts.is_empty() {
            return Ok(Self {
                wallet_info,
                wallet_accounts,
                states_by_node: HashMap::new(),
            });
        }

        let scanner_groups = {
            let scanner_state =
                world
                    .wallet_scanner_state
                    .lock()
                    .map_err(|_| StepError::LogicalError {
                        message: "Wallet scanner state lock poisoned while preparing snapshot"
                            .to_owned(),
                    })?;
            scanner_state
                .groups
                .values()
                .filter(|group| group.wallet_count > 0)
                .cloned()
                .collect::<Vec<_>>()
        };
        let scanner_wallets = world.with_wallets(TrackedWallets::to_state)?;

        let mut states_by_node = HashMap::new();
        for group in scanner_groups {
            let tip = group.applied_tip.ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "wallet scanner group `{}` has no applied tip for snapshot",
                    group.group_id
                ),
            })?;
            let slot = group.applied_slot.ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "wallet scanner group `{}` has no applied slot for snapshot",
                    group.group_id
                ),
            })?;
            let tip = tip.to_string();
            let group_nodes = scanner_group_node_names(world, &group.group_id);
            for node_name in group_nodes {
                let mut node_snapshot = WalletNodeSnapshot {
                    tip: tip.clone(),
                    height: group.applied_height,
                    slot: Some(slot),
                    tracked_wallets: scanner_wallets.clone(),
                };
                filter_node_snapshot_wallets(world, &node_name, &mut node_snapshot)?;
                states_by_node.insert(node_name, node_snapshot);
            }
        }

        if states_by_node.is_empty() && !wallet_info.is_empty() {
            return Err(StepError::LogicalError {
                message: "wallet scanner state has no exportable node snapshots".to_owned(),
            });
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

    fn apply_metadata(&self, world: &mut CucumberWorld) {
        world.wallet_info.clone_from(&self.wallet_info);
        world.wallet_accounts.clone_from(&self.wallet_accounts);
    }

    async fn apply_for_node(
        &self,
        snapshot_node_name: &str,
        runtime_node_name: &str,
        client: &NodeHttpClient,
        world: &mut CucumberWorld,
    ) -> StepResult {
        let Some(node_snapshot) = self
            .states_by_node
            .get(runtime_node_name)
            .or_else(|| self.states_by_node.get(snapshot_node_name))
        else {
            return Err(StepError::LogicalError {
                message: format!(
                    "wallet snapshot does not contain state for runtime node `{runtime_node_name}` \
                     or snapshot source node `{snapshot_node_name}`"
                ),
            });
        };

        let runtime_wallet_ids = wallet_ids_for_source(world, runtime_node_name)?;
        let runtime_wallet_utxos = node_snapshot
            .tracked_wallets
            .to_wallet_utxos()
            .into_iter()
            .filter(|(wallet_id, _)| runtime_wallet_ids.contains(wallet_id))
            .collect::<WalletUtxos>();
        let tip = parse_header_id(&node_snapshot.tip)?;
        let slot = match node_snapshot.slot {
            Some(slot) => slot,
            None => fetch_tip_slot(client, &tip).await?,
        };

        world.with_wallets_mut(|wallets| {
            wallets.record_header_height(
                runtime_node_name,
                &node_snapshot.tip,
                node_snapshot.height,
            );
            wallets.record_observed_wallets_utxos(
                node_snapshot.tip.clone(),
                runtime_wallet_utxos
                    .iter()
                    .map(|(wallet_id, utxos)| (wallet_id.clone(), utxos.clone())),
            );
            wallets.replace_current_wallets_utxos(runtime_wallet_utxos.clone());
        })?;
        world.wallet_scanner_seeds.insert(
            runtime_node_name.to_owned(),
            ScannerSeed::Snapshot {
                wallet_utxos: runtime_wallet_utxos,
                tip,
                height: node_snapshot.height,
                slot,
                source_node_names: vec![runtime_node_name.to_owned()],
                rescan_blocks: DEFAULT_SCANNER_SNAPSHOT_RESCAN_BLOCKS,
            },
        );

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

/// Prepare Cucumber wallet state from the synchronized wallet scanner before
/// node shutdown.
///
/// Use this for snapshot-on-stop flows. It captures all wallets' state against
/// synchronized wallet scanner state so saved node snapshots do not constrain
/// wallet artifact creation.
pub async fn prepare_all_wallets_snapshot(world: &mut CucumberWorld) -> StepResult {
    world
        .wait_for_wallet_scanner_catch_up(Duration::from_secs(30))
        .await?;
    let snapshot = WalletSnapshot::from_scanner_state(world)?;
    world.snapshot_save_config.prepared_wallet_snapshot = Some(snapshot);

    Ok(())
}

/// Save the wallet snapshot prepared before node shutdown.
///
/// This is the final half of snapshot-on-stop. It fails if no wallet snapshot
/// was prepared.
pub fn save_prepared_all_wallets_snapshot(
    snapshot_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    let Some(snapshot) = world.snapshot_save_config.prepared_wallet_snapshot.take() else {
        return Err(StepError::LogicalError {
            message: format!("wallet snapshot `{snapshot_name}` was not prepared before shutdown"),
        });
    };

    save_all_wallets_snapshot_value(snapshot_name, snapshot)
}

/// Save Cucumber wallet state from synchronized wallet scanner state.
///
/// Wallet snapshots are now group-wide scanner-state artifacts, not
/// selected-node artifacts. Scanner catch-up is the wallet snapshot
/// synchronization boundary.
pub async fn create_and_save_all_wallets_snapshot(
    snapshot_name: &str,
    world: &mut CucumberWorld,
) -> StepResult {
    world
        .wait_for_wallet_scanner_catch_up(Duration::from_secs(30))
        .await?;
    let snapshot = WalletSnapshot::from_scanner_state(world)?;
    save_all_wallets_snapshot_value(snapshot_name, snapshot)
}

fn save_all_wallets_snapshot_value(snapshot_name: &str, snapshot: WalletSnapshot) -> StepResult {
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
/// `snapshot_node_name` is the source-node entry inside the snapshot, not
/// necessarily the runtime node being started. This supports the common restore
/// shape where several fresh nodes all start from one saved node snapshot.
pub async fn restore_wallet_snapshot_if_present(
    snapshot_name: &str,
    snapshot_node_name: &str,
    runtime_node_name: &str,
    client: &NodeHttpClient,
    world: &mut CucumberWorld,
) -> StepResult {
    let Some(snapshot) = read_wallet_snapshot_if_present(snapshot_name)? else {
        return Ok(());
    };

    snapshot
        .apply_for_node(snapshot_node_name, runtime_node_name, client, world)
        .await?;
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

fn filter_node_snapshot_wallets(
    world: &CucumberWorld,
    node_name: &str,
    node_snapshot: &mut WalletNodeSnapshot,
) -> StepResult {
    let wallet_ids = wallet_ids_for_source(world, node_name)?;
    node_snapshot.tracked_wallets = node_snapshot
        .tracked_wallets
        .filtered_to_wallets(&wallet_ids);
    Ok(())
}

fn wallet_ids_for_source(
    world: &CucumberWorld,
    source_node_name: &str,
) -> Result<HashSet<WalletId>, StepError> {
    Ok(world
        .wallet_tracking_keys_for_source(source_node_name)?
        .into_iter()
        .map(|keys| keys.wallet_id().clone())
        .collect())
}

fn scanner_group_node_names(world: &CucumberWorld, group_id: &str) -> Vec<String> {
    if world.node_groups.is_empty() {
        return world.nodes_info.keys().cloned().collect();
    }

    world
        .node_groups
        .get(group_id)
        .map(|nodes| nodes.iter().cloned().collect())
        .unwrap_or_default()
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

fn clear_wallet_snapshot_state(world: &mut CucumberWorld) -> StepResult {
    world.wallet_info.clear();
    world.wallet_accounts.clear();
    world.fee_state.clear_reservations();
    world.with_wallets_mut(|wallets| {
        wallets.replace_from_state(TrackedWalletsState::default());
    })?;

    world.reset_wallet_scanner();
    world.wallet_scanner_seeds.clear();
    world.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

    Ok(())
}

async fn fetch_tip_slot(client: &NodeHttpClient, tip: &HeaderId) -> Result<u64, StepError> {
    let Some(block) = client.block(tip).await? else {
        return Err(StepError::LogicalError {
            message: format!("wallet snapshot tip `{tip}` is not available from restored node"),
        });
    };

    Ok(u64::from(block.header.slot))
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
