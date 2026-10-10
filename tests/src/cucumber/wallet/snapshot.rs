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
        scanner::{
            accounting::ScannerAccountingSnapshot,
            config::{DEFAULT_SCANNER_SNAPSHOT_RESCAN_BLOCKS, ScannerSeed},
            state::ScannerStateCheckpoint,
        },
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
    /// Full scanner seed, including service-locked UTXOs and lock markers.
    /// Optional only so legacy artifacts can be rejected with a clear error.
    #[serde(default)]
    accounting: Option<ScannerAccountingSnapshot>,
    /// Older scanner checkpoints, newest first, used as fallback seed
    /// positions when the snapshot tip is not found on the restored chain.
    #[serde(default)]
    checkpoints: Vec<WalletSnapshotCheckpoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WalletSnapshotCheckpoint {
    tip: String,
    height: u64,
    slot: u64,
    tracked_wallets: TrackedWalletsState,
    /// Full scanner seed for correct rollback across a restored snapshot.
    #[serde(default)]
    accounting: Option<ScannerAccountingSnapshot>,
}

impl WalletSnapshot {
    /// Build a wallet snapshot from the scanner-applied group tips.
    ///
    /// Scanner catch-up has already selected and applied the canonical group
    /// tips, so this avoids racing against fresh per-node consensus queries
    /// while nodes are still running.
    fn from_scanner_state(world: &CucumberWorld) -> Result<Self, StepError> {
        let wallet_info = world.wallet_registry.wallet_info.clone();
        let wallet_accounts = world.wallet_registry.wallet_accounts.clone();
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
                    .scanner
                    .state
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
            let accounting = group
                .recent_checkpoints
                .iter()
                .find(|checkpoint| checkpoint.tip == tip)
                .map(|checkpoint| checkpoint.accounting.clone())
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "wallet scanner group `{}` has no accounting checkpoint at its applied tip",
                        group.group_id
                    ),
                })?;
            let tip = tip.to_string();
            let checkpoints = group
                .recent_checkpoints
                .iter()
                .map(|checkpoint| WalletSnapshotCheckpoint {
                    tip: checkpoint.tip.to_string(),
                    height: checkpoint.height,
                    slot: checkpoint.slot,
                    tracked_wallets: TrackedWalletsState::from_wallet_utxos(
                        checkpoint.wallet_utxos.clone(),
                    ),
                    accounting: Some(checkpoint.accounting.clone()),
                })
                .collect::<Vec<_>>();
            let group_nodes = scanner_group_node_names(world, &group.group_id);
            for node_name in group_nodes {
                let mut node_snapshot = WalletNodeSnapshot {
                    tip: tip.clone(),
                    height: group.applied_height,
                    slot: Some(slot),
                    tracked_wallets: scanner_wallets.clone(),
                    accounting: Some(accounting.clone()),
                    checkpoints: checkpoints.clone(),
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
        world
            .wallet_registry
            .wallet_info
            .clone_from(&self.wallet_info);
        world
            .wallet_registry
            .wallet_accounts
            .clone_from(&self.wallet_accounts);
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
        let accounting = Box::new(
            require_scanner_accounting(
                node_snapshot.accounting.as_ref(),
                &format!("node `{runtime_node_name}` snapshot"),
            )?
            .filtered_for_wallets(&runtime_wallet_ids),
        );
        let tip = parse_header_id(&node_snapshot.tip)?;
        let slot = match node_snapshot.slot {
            Some(slot) => slot,
            None => fetch_tip_slot(client, &tip).await?,
        };
        let fallback_checkpoints = node_snapshot
            .checkpoints
            .iter()
            .map(|checkpoint| {
                restore_fallback_checkpoint(checkpoint, runtime_node_name, &runtime_wallet_ids)
            })
            .collect::<Result<Vec<_>, StepError>>()?
            .into_iter()
            .filter(|checkpoint| checkpoint.tip.to_string() != node_snapshot.tip)
            .collect();

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
        world.scanner.seeds.insert(
            runtime_node_name.to_owned(),
            ScannerSeed::Snapshot {
                accounting,
                tip,
                height: node_snapshot.height,
                slot,
                source_node_names: vec![runtime_node_name.to_owned()],
                rescan_blocks: DEFAULT_SCANNER_SNAPSHOT_RESCAN_BLOCKS,
                fallback_checkpoints,
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
        let snapshot: Self = serde_json::from_value(artifact.payload.clone())?;
        snapshot
            .validate_scanner_accounting()
            .map_err(|error| -> DynError { Box::new(error) })?;
        Ok(snapshot)
    }

    fn validate_scanner_accounting(&self) -> Result<(), StepError> {
        for (node_name, node_snapshot) in &self.states_by_node {
            require_scanner_accounting(
                node_snapshot.accounting.as_ref(),
                &format!("node `{node_name}` snapshot"),
            )?;
            for (index, checkpoint) in node_snapshot.checkpoints.iter().enumerate() {
                require_scanner_accounting(
                    checkpoint.accounting.as_ref(),
                    &format!("node `{node_name}` fallback checkpoint {index}"),
                )?;
            }
        }
        Ok(())
    }
}

fn require_scanner_accounting<'a>(
    accounting: Option<&'a ScannerAccountingSnapshot>,
    location: &str,
) -> Result<&'a ScannerAccountingSnapshot, StepError> {
    accounting.ok_or_else(|| StepError::LogicalError {
        message: format!(
            "wallet {location} is missing scanner accounting state; refusing to resume because \
             SDP lock state cannot be reconstructed from spendable wallet UTXOs"
        ),
    })
}

fn restore_fallback_checkpoint(
    checkpoint: &WalletSnapshotCheckpoint,
    runtime_node_name: &str,
    runtime_wallet_ids: &HashSet<WalletId>,
) -> Result<ScannerStateCheckpoint, StepError> {
    let wallet_utxos = checkpoint
        .tracked_wallets
        .to_wallet_utxos()
        .into_iter()
        .filter(|(wallet_id, _)| runtime_wallet_ids.contains(wallet_id))
        .collect();
    let accounting = require_scanner_accounting(
        checkpoint.accounting.as_ref(),
        &format!(
            "node `{runtime_node_name}` fallback checkpoint at {}",
            checkpoint.tip
        ),
    )?
    .filtered_for_wallets(runtime_wallet_ids);

    Ok(ScannerStateCheckpoint {
        wallet_utxos,
        accounting,
        tip: parse_header_id(&checkpoint.tip)?,
        height: checkpoint.height,
        slot: checkpoint.slot,
    })
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
    world.snapshots.save.prepared_wallet_snapshot = Some(snapshot);

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
    let Some(snapshot) = world.snapshots.save.prepared_wallet_snapshot.take() else {
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
        .save_provider_artifact(snapshot_name, WALLET_SNAPSHOT_EXTENSION_ID, artifact)
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
    world.scanner.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

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
    world.scanner.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

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
    node_snapshot.accounting = node_snapshot
        .accounting
        .as_ref()
        .map(|accounting| accounting.filtered_for_wallets(&wallet_ids));
    for checkpoint in &mut node_snapshot.checkpoints {
        checkpoint.tracked_wallets = checkpoint.tracked_wallets.filtered_to_wallets(&wallet_ids);
        checkpoint.accounting = checkpoint
            .accounting
            .as_ref()
            .map(|accounting| accounting.filtered_for_wallets(&wallet_ids));
    }
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
    if world.fork_groups.groups().is_empty() {
        return world.nodes_info.keys().cloned().collect();
    }

    world
        .fork_groups
        .groups()
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
        .providers
        .get(WALLET_SNAPSHOT_EXTENSION_ID)
        .cloned();

    artifact
        .map(|artifact| WalletSnapshot::from_artifact(&artifact).map_err(|e| snapshot_error(&e)))
        .transpose()
}

fn clear_wallet_snapshot_state(world: &mut CucumberWorld) -> StepResult {
    world.wallet_registry.wallet_info.clear();
    world.wallet_registry.wallet_accounts.clear();
    world.wallet_registry.fee_state.clear_reservations();
    world.with_wallets_mut(|wallets| {
        wallets.replace_from_state(TrackedWalletsState::default());
    })?;

    world.reset_wallet_scanner();
    world.scanner.seeds.clear();
    world.scanner.observed_transaction_hashes = Arc::new(Mutex::new(HashSet::new()));

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

#[cfg(test)]
mod tests {
    use lb_common_http_client::{ApiBlock, ApiHeader, Slot};
    use lb_core::{
        events::{Event, Events, HeaderEvent},
        header::ContentId,
        mantle::{Note, Utxo},
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_key_management_system_service::keys::ZkPublicKey;
    use serde_json::json;

    use super::*;
    use crate::common::wallet::{TrackedWalletKeys, scanner::accounting::ScannerAccounting};

    fn wallet_node_snapshot(
        accounting: Option<ScannerAccountingSnapshot>,
        checkpoints: Vec<WalletSnapshotCheckpoint>,
    ) -> WalletNodeSnapshot {
        WalletNodeSnapshot {
            tip: "tip".to_owned(),
            height: 1,
            slot: Some(1),
            tracked_wallets: TrackedWalletsState::default(),
            accounting,
            checkpoints,
        }
    }

    fn artifact_for_node(node: WalletNodeSnapshot) -> SnapshotArtifact {
        let snapshot = WalletSnapshot {
            wallet_info: WalletInfoMap::default(),
            wallet_accounts: HashMap::new(),
            states_by_node: HashMap::from([("NODE".to_owned(), node)]),
        };
        let payload = serde_json::to_value(snapshot).expect("snapshot serializes");
        SnapshotArtifact::new(2, json!({}), payload)
    }

    fn empty_block(seed: u8) -> ApiBlock {
        ApiBlock {
            header: ApiHeader {
                id: HeaderId::from([seed; 32]),
                parent_block: HeaderId::from([seed.saturating_sub(1); 32]),
                slot: Slot::from(u64::from(seed)),
                body_root: ContentId::from([0; 32]),
                proof_of_leadership: Groth16LeaderProof::genesis(),
            },
            uncle_headers: Vec::new(),
            transactions: Vec::new(),
        }
    }

    #[test]
    fn current_snapshot_restores_locked_note_until_unlock_event() {
        let wallet_id = WalletId::new("alice");
        let wallet_pk = ZkPublicKey::new(1.into());
        let locked = Utxo::new([1; 32], 0, Note::new(10, wallet_pk));
        let note_id = locked.id();
        let accounting = ScannerAccountingSnapshot {
            wallet_utxos: HashMap::from([(wallet_id, vec![locked])]),
            locked_service_note_ids: HashSet::from([note_id]),
        };
        let artifact = artifact_for_node(wallet_node_snapshot(Some(accounting), Vec::new()));
        let restored = WalletSnapshot::from_artifact(&artifact).expect("current snapshot restores");
        let accounting = require_scanner_accounting(
            restored.states_by_node["NODE"].accounting.as_ref(),
            "test node snapshot",
        )
        .expect("current-format snapshot has accounting")
        .clone();
        let mut scanner = ScannerAccounting::from_snapshot(
            vec![TrackedWalletKeys::new("alice", [wallet_pk])],
            accounting,
        )
        .expect("scanner restores full accounting");
        assert_eq!(scanner.wallet_utxos()["alice"], []);

        let unlocked = Events::from(Event::Header(HeaderEvent::SdpNoteUnlocked {
            note_id,
            service_type: lb_core::sdp::ServiceType::BlendNetwork,
            declaration_id: lb_core::sdp::DeclarationId([2; 32]),
        }));
        scanner.apply_block_with_events(&empty_block(1), &unlocked);
        assert_eq!(scanner.wallet_utxos()["alice"], vec![locked]);
    }

    #[test]
    fn restored_fallback_keeps_spendable_and_full_accounting_views_distinct() {
        let wallet_id = WalletId::new("alice");
        let wallet_pk = ZkPublicKey::new(1.into());
        let spendable = Utxo::new([1; 32], 0, Note::new(10, wallet_pk));
        let locked = Utxo::new([2; 32], 0, Note::new(20, wallet_pk));
        let locked_note_id = locked.id();
        let checkpoint = WalletSnapshotCheckpoint {
            tip: HeaderId::from([3; 32]).to_string(),
            height: 2,
            slot: 2,
            tracked_wallets: TrackedWalletsState::from_wallet_utxos(HashMap::from([(
                wallet_id.clone(),
                vec![spendable],
            )])),
            accounting: Some(ScannerAccountingSnapshot {
                wallet_utxos: HashMap::from([(wallet_id.clone(), vec![spendable, locked])]),
                locked_service_note_ids: HashSet::from([locked_note_id]),
            }),
        };

        let restored =
            restore_fallback_checkpoint(&checkpoint, "NODE", &HashSet::from([wallet_id.clone()]))
                .expect("fallback checkpoint restores with full accounting");

        assert_eq!(restored.wallet_utxos[&wallet_id], vec![spendable]);
        assert_eq!(
            restored.accounting.wallet_utxos[&wallet_id],
            vec![spendable, locked]
        );
        assert!(
            restored
                .accounting
                .locked_service_note_ids
                .contains(&locked_note_id)
        );
    }

    #[test]
    fn legacy_top_level_snapshot_without_accounting_fails_closed() {
        let artifact = artifact_for_node(wallet_node_snapshot(None, Vec::new()));
        let mut payload = artifact.payload;
        payload["states_by_node"]["NODE"]
            .as_object_mut()
            .expect("node snapshot is an object")
            .remove("accounting");
        let legacy_artifact = SnapshotArtifact::new(2, json!({}), payload);

        let error = WalletSnapshot::from_artifact(&legacy_artifact)
            .expect_err("legacy snapshot cannot resume without lock accounting");
        assert!(
            error
                .to_string()
                .contains("missing scanner accounting state")
        );
        assert!(
            error
                .to_string()
                .contains("SDP lock state cannot be reconstructed")
        );
    }

    #[test]
    fn legacy_fallback_checkpoint_without_accounting_fails_closed() {
        let checkpoint = WalletSnapshotCheckpoint {
            tip: "older-tip".to_owned(),
            height: 0,
            slot: 0,
            tracked_wallets: TrackedWalletsState::default(),
            accounting: None,
        };
        let artifact = artifact_for_node(wallet_node_snapshot(
            Some(ScannerAccountingSnapshot::default()),
            vec![checkpoint],
        ));
        let mut payload = artifact.payload;
        payload["states_by_node"]["NODE"]["checkpoints"][0]
            .as_object_mut()
            .expect("fallback checkpoint is an object")
            .remove("accounting");
        let legacy_artifact = SnapshotArtifact::new(2, json!({}), payload);

        let error = WalletSnapshot::from_artifact(&legacy_artifact)
            .expect_err("legacy fallback cannot resume without lock accounting");
        assert!(error.to_string().contains("fallback checkpoint 0"));
        assert!(
            error
                .to_string()
                .contains("missing scanner accounting state")
        );
    }
}
