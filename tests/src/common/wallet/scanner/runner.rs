use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use lb_core::{header::HeaderId, mantle::TxHash};
use lb_testing_framework::NodeHttpClient;
use tokio::{task::JoinHandle, time::sleep};
use tracing::{info, warn};

use super::{
    accounting::ScannerAccounting,
    block_source::stream_blocks_range,
    config::{ForkGroupScannerConfig, ScannerSeed},
    state::{ScannerStatus, SharedWalletScannerState},
};
use crate::{
    common::wallet::{
        TrackedWallets, WalletId,
        scanner::{error::ScannerError, state::ForkGroupScannerState},
    },
    cucumber::{
        error::StepError,
        wallet::best_node::{BestGroupNode, display_group_key},
    },
};

const TARGET: &str = "cucumber_wallet";
const MIN_SCANNER_CHECKPOINTS: usize = 8;
const MAX_SCANNER_CHECKPOINTS: usize = 128;
const SCANNER_ROLLBACK_BLOCKS: u64 = 5;

#[derive(Clone)]
struct ScannerCheckpoint {
    accounting: ScannerAccounting,
    applied_height: u64,
    applied_tip: Option<HeaderId>,
    applied_slot: Option<u64>,
    source_node_names: Vec<String>,
}

impl ScannerCheckpoint {
    fn new(
        accounting: &ScannerAccounting,
        applied_height: u64,
        applied_tip: Option<HeaderId>,
        applied_slot: Option<u64>,
        source_node_names: Vec<String>,
    ) -> Self {
        Self {
            accounting: accounting.clone(),
            applied_height,
            applied_tip,
            applied_slot,
            source_node_names,
        }
    }
}

struct SnapshotRescan {
    tip: HeaderId,
    slot: u64,
    blocks: u64,
}

struct ScannerInitialState {
    accounting: ScannerAccounting,
    applied_height: u64,
    applied_tip: Option<HeaderId>,
    applied_slot: Option<u64>,
    source_node_names: Vec<String>,
}

#[derive(Debug)]
enum ScannerIterationError {
    Step(StepError),
    Continuity(StepError),
}

impl From<StepError> for ScannerIterationError {
    fn from(error: StepError) -> Self {
        Self::Step(error)
    }
}

/// Handles and shutdown state for active wallet scanner tasks.
pub struct WalletScannerRuntime {
    /// Scanner task handles, one per fork group with tracked wallets.
    pub handles: Vec<JoinHandle<()>>,
    shutdown_requested: Arc<AtomicBool>,
}

impl WalletScannerRuntime {
    /// Immediate cancellation. Keeps the old behavior for synchronous callers.
    pub fn cancel(self) {
        for handle in self.handles {
            handle.abort();
        }
    }

    /// Graceful cancellation. Lets the current scanner iteration complete,
    /// then stops before starting the next iteration.
    pub async fn shutdown_after_current_iteration(self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);

        for handle in self.handles {
            if let Err(error) = handle.await
                && !error.is_cancelled()
            {
                warn!(
                    target: TARGET,
                    "wallet scanner task failed during graceful shutdown: {error}"
                );
            }
        }
    }
}

#[must_use]
/// Start one scanner task per fork-group config.
pub fn start_wallet_scanners(configs: Vec<ForkGroupScannerConfig>) -> WalletScannerRuntime {
    let mut handles = Vec::new();
    let shutdown_requested = Arc::new(AtomicBool::new(false));

    for config in configs {
        handles.push(tokio::spawn(run_group_scanner(
            config,
            Arc::clone(&shutdown_requested),
        )));
    }

    WalletScannerRuntime {
        handles,
        shutdown_requested,
    }
}

/// Wait until all scanner groups have applied their latest selected target
/// heights.
pub async fn wait_for_scanner_catch_up(
    state: &SharedWalletScannerState,
    timeout: Duration,
) -> Result<(), StepError> {
    let started_at = tokio::time::Instant::now();

    loop {
        let waiting = {
            let snapshot = state.lock().unwrap_or_else(PoisonError::into_inner);
            snapshot
                .groups
                .values()
                .filter(|group| group.wallet_count > 0)
                .any(scanner_group_needs_catch_up)
        };

        if !waiting {
            return Ok(());
        }

        if started_at.elapsed() >= timeout {
            let status = {
                let snapshot = state.lock().unwrap_or_else(PoisonError::into_inner);
                snapshot
                    .groups
                    .values()
                    .map(|group| {
                        format!(
                            "{}: source={:?} applied={} target={:?} status={:?} last_error={:?}",
                            display_group_key(&group.group_id),
                            group.source_node,
                            group.applied_height,
                            group.target_height,
                            group.status,
                            group.last_error
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | ")
            };
            return Err(StepError::Timeout {
                message: format!("wallet scanner catch-up timeout: {status}"),
            });
        }

        sleep(Duration::from_millis(250)).await;
    }
}

fn scanner_group_needs_catch_up(group: &ForkGroupScannerState) -> bool {
    group.status == ScannerStatus::Error
        || group.last_error.is_some()
        || group
            .target_height
            .is_none_or(|target_height| group.applied_height < target_height)
}

fn initialize_scanner_from_seed(
    config: &ForkGroupScannerConfig,
) -> Result<ScannerInitialState, StepError> {
    match &config.seed {
        ScannerSeed::Genesis => {
            let accounting = ScannerAccounting::new(
                config.wallet_keys.clone(),
                &config.genesis_utxos,
            )
            .map_err(|error| StepError::LogicalError {
                message: format!("wallet scanner accounting initialization failed: {error}"),
            })?;
            Ok(ScannerInitialState {
                accounting,
                applied_height: 0,
                applied_tip: None,
                applied_slot: None,
                source_node_names: Vec::new(),
            })
        }
        ScannerSeed::Snapshot {
            wallet_utxos,
            tip,
            height,
            slot,
            source_node_names,
            ..
        } => {
            let accounting = ScannerAccounting::from_wallet_utxos(
                config.wallet_keys.clone(),
                wallet_utxos.clone(),
            )
            .map_err(|error| StepError::LogicalError {
                message: format!("wallet scanner accounting initialization failed: {error}"),
            })?;
            Ok(ScannerInitialState {
                accounting,
                applied_height: *height,
                applied_tip: Some(*tip),
                applied_slot: Some(*slot),
                source_node_names: source_node_names.clone(),
            })
        }
    }
}

fn snapshot_rescan_from_seed(seed: &ScannerSeed) -> Option<SnapshotRescan> {
    let ScannerSeed::Snapshot {
        tip,
        slot,
        rescan_blocks,
        ..
    } = seed
    else {
        return None;
    };

    (*rescan_blocks > 0).then_some(SnapshotRescan {
        tip: *tip,
        slot: *slot,
        blocks: *rescan_blocks,
    })
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Scanner task state machine keeps retry/reset behavior in one loop."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Scanner task state machine keeps retry/reset behavior in one loop."
)]
async fn run_group_scanner(config: ForkGroupScannerConfig, shutdown_requested: Arc<AtomicBool>) {
    info!(
        target: TARGET,
        "wallet scanner started for group '{}' with representative wallet '{}'",
        display_group_key(&config.group_id),
        config.representative_wallet_name
    );

    let initial_state = match initialize_scanner_from_seed(&config) {
        Ok(initial) => initial,
        Err(error) => {
            warn!(
                target: TARGET,
                "wallet scanner group '{}' failed to initialize: {error}",
                display_group_key(&config.group_id),
            );
            update_group_state(&config.scanner_state, &config.group_id, |group| {
                group.status = ScannerStatus::Error;
                group.last_error = Some(error.to_string());
            });
            return;
        }
    };
    let mut accounting = initial_state.accounting;
    let mut applied_height = initial_state.applied_height;
    let mut applied_tip = initial_state.applied_tip;
    let mut applied_slot = initial_state.applied_slot;
    let source_node_names = initial_state.source_node_names;
    let mut snapshot_rescan = snapshot_rescan_from_seed(&config.seed);
    let mut checkpoints = VecDeque::new();
    let mut last_msg = String::new();
    if let Some(applied_tip) = applied_tip
        && let Err(error) = publish_wallet_state(
            &config.wallets,
            &source_node_names,
            applied_height,
            applied_tip.to_string(),
            accounting.wallet_utxos(),
        )
    {
        warn!(
            target: TARGET,
            "wallet scanner group '{}' failed to publish seed state: {error}",
            display_group_key(&config.group_id),
        );
    }
    push_checkpoint(
        &mut checkpoints,
        ScannerCheckpoint::new(
            &accounting,
            applied_height,
            applied_tip,
            applied_slot,
            source_node_names,
        ),
    );

    loop {
        if check_shutdown(&shutdown_requested, &config.group_id) {
            return;
        }

        let result = scan_group_once(
            &config,
            &mut accounting,
            &mut applied_height,
            &mut applied_tip,
            &mut applied_slot,
            &mut checkpoints,
            &mut snapshot_rescan,
            &mut last_msg,
        )
        .await;
        if let Err(error) = result {
            warn!(
                target: TARGET,
                "wallet scanner group '{}' failed: {}",
                display_group_key(&config.group_id),
                scanner_iteration_error_message(&error),
            );
            update_group_state(&config.scanner_state, &config.group_id, |group| {
                group.status = ScannerStatus::Error;
                group.last_error = Some(scanner_iteration_error_message(&error));
            });
            if matches!(error, ScannerIterationError::Continuity(_)) {
                if let Some(checkpoint) = rollback_checkpoint(&checkpoints, applied_height)
                    .filter(|checkpoint| checkpoint.applied_height < applied_height)
                {
                    warn!(
                        target: TARGET,
                        "wallet scanner group '{}' rolling back to height {} after continuity failure",
                        display_group_key(&config.group_id),
                        checkpoint.applied_height,
                    );
                    accounting = checkpoint.accounting;
                    applied_height = checkpoint.applied_height;
                    applied_tip = checkpoint.applied_tip;
                    applied_slot = checkpoint.applied_slot;
                    let source_node_names = checkpoint.source_node_names;
                    checkpoints.retain(|checkpoint| checkpoint.applied_height <= applied_height);
                    if let Some(applied_tip) = applied_tip
                        && !source_node_names.is_empty()
                        && let Err(error) = publish_wallet_state(
                            &config.wallets,
                            &source_node_names,
                            applied_height,
                            applied_tip.to_string(),
                            accounting.wallet_utxos(),
                        )
                    {
                        warn!(
                            target: TARGET,
                            "wallet scanner group '{}' failed to publish rollback state: {error}",
                            display_group_key(&config.group_id),
                        );
                    }
                } else if let Err(error) = reset_scanner_to_genesis(
                    &config,
                    &mut accounting,
                    &mut applied_height,
                    &mut applied_tip,
                    &mut applied_slot,
                    &mut checkpoints,
                    &mut snapshot_rescan,
                ) {
                    warn!(
                        target: TARGET,
                        "wallet scanner group '{}' failed to reset to genesis after continuity failure: {error}",
                        display_group_key(&config.group_id),
                    );
                }
            }
        }

        if sleep_or_shutdown(&shutdown_requested, config.poll_interval, &config.group_id).await {
            return;
        }
    }
}

async fn sleep_or_shutdown(
    shutdown_requested: &Arc<AtomicBool>,
    poll_interval: Duration,
    group_id: &str,
) -> bool {
    let sleep = sleep(poll_interval.max(Duration::from_millis(100)));
    tokio::pin!(sleep);

    loop {
        tokio::select! {
            () = &mut sleep => return false,
            () = tokio::time::sleep(Duration::from_millis(25)) => {
                if check_shutdown(shutdown_requested, group_id) {
                    return true;
                }
            }
        }
    }
}

fn check_shutdown(shutdown_requested: &Arc<AtomicBool>, group_id: &str) -> bool {
    if shutdown_requested.load(Ordering::Relaxed) {
        info!(
            target: TARGET,
            "wallet scanner stopping for group '{}'",
            display_group_key(group_id),
        );
        return true;
    }
    false
}

fn reset_scanner_to_genesis(
    config: &ForkGroupScannerConfig,
    accounting: &mut ScannerAccounting,
    applied_height: &mut u64,
    applied_tip: &mut Option<HeaderId>,
    applied_slot: &mut Option<u64>,
    checkpoints: &mut VecDeque<ScannerCheckpoint>,
    snapshot_rescan: &mut Option<SnapshotRescan>,
) -> Result<(), StepError> {
    warn!(
        target: TARGET,
        "wallet scanner group '{}' exhausted continuity checkpoints; resetting to genesis replay",
        display_group_key(&config.group_id),
    );

    *accounting = ScannerAccounting::new(config.wallet_keys.clone(), &config.genesis_utxos)
        .map_err(|error| StepError::LogicalError {
            message: format!("wallet scanner accounting reset to genesis failed: {error}"),
        })?;
    *applied_height = 0;
    *applied_tip = None;
    *applied_slot = None;
    *snapshot_rescan = None;
    checkpoints.clear();
    push_checkpoint(
        checkpoints,
        ScannerCheckpoint::new(
            accounting,
            *applied_height,
            *applied_tip,
            *applied_slot,
            Vec::new(),
        ),
    );

    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "One scanner iteration keeps source selection, range application, and publishing ordered."
)]
#[expect(
    clippy::too_many_arguments,
    reason = "One scanner iteration owns explicit mutable scanner-loop state."
)]
async fn scan_group_once(
    config: &ForkGroupScannerConfig,
    accounting: &mut ScannerAccounting,
    applied_height: &mut u64,
    applied_tip: &mut Option<HeaderId>,
    applied_slot: &mut Option<u64>,
    checkpoints: &mut VecDeque<ScannerCheckpoint>,
    snapshot_rescan: &mut Option<SnapshotRescan>,
    last_msg: &mut String,
) -> Result<(), ScannerIterationError> {
    update_group_state(&config.scanner_state, &config.group_id, |group| {
        group.status = ScannerStatus::WaitingForSource;
        group.last_error = None;
    });

    let best_node_info = (config.best_node_selector)(
        config.representative_wallet_name.clone(),
        config.group_nodes.clone(),
        config.node_clients.clone(),
        config.wallet_to_node.clone(),
        config.group_id.clone(),
        last_msg,
    )
    .await?;

    let best_node = best_node_info.best_node_for_group(&config.group_id)?;
    let client = config.node_client_for_group(&best_node.node_name)?;

    update_group_state(&config.scanner_state, &config.group_id, |group| {
        group.source_node = Some(best_node.node_name.clone());
        group.target_height = Some(best_node.height);
        group.target_slot = Some(best_node.slot);
        group.target_tip = parse_header_id(&best_node.tip);
        group.status = if *applied_height == 0 {
            ScannerStatus::Backfilling
        } else {
            ScannerStatus::Tailing
        };
    });

    let published_genesis = publish_genesis_wallet_state_if_needed(
        config,
        accounting,
        best_node,
        applied_height,
        applied_tip,
        applied_slot,
        checkpoints,
    )?;

    if let Some(rescan) = snapshot_rescan.as_ref() {
        verify_snapshot_rescan(config, accounting, client, best_node, rescan).await?;
        *snapshot_rescan = None;
    }

    let from_slot = applied_slot.map_or(0, |slot| slot.saturating_add(1));

    let observed_tx_hashes_before = accounting.observed_transaction_hashes().clone();
    let observed_tx_count_before = observed_tx_hashes_before.len();
    let mut applied_block_count = 0usize;
    let mut skipped_unconnected_block_count = 0usize;
    let mut last_publish_height = *applied_height;
    let mut continuity_error = None;
    let mut connected_to_tip = applied_tip.is_none();
    let initial_applied_tip = *applied_tip;

    let streamed_block_count = stream_blocks_range(client, from_slot, best_node.slot, |block| {
        if continuity_error.is_some() {
            return Ok(());
        }

        if let Some(expected_parent) = *applied_tip
            && block.header.parent_block != expected_parent
        {
            if !connected_to_tip {
                skipped_unconnected_block_count += 1;
                return Ok(());
            }

            continuity_error = Some(wallet_scanner_continuity_error(
                config,
                block.header.parent_block,
                expected_parent,
                skipped_unconnected_block_count,
            ));
            return Ok(());
        }

        connected_to_tip = true;
        accounting.apply_block(&block);
        *applied_height = applied_height.saturating_add(1);
        *applied_tip = Some(block.header.id);
        *applied_slot = Some(u64::from(block.header.slot));
        applied_block_count += 1;

        push_checkpoint(
            checkpoints,
            ScannerCheckpoint::new(
                accounting,
                *applied_height,
                *applied_tip,
                *applied_slot,
                best_node.same_tip_nodes.clone(),
            ),
        );

        let wallet_utxos = accounting.wallet_utxos();
        if *applied_height >= best_node.height
            || (*applied_height).saturating_sub(last_publish_height) >= config.range_batch_size
        {
            publish_wallet_state(
                &config.wallets,
                &best_node.same_tip_nodes,
                *applied_height,
                block.header.id.to_string(),
                wallet_utxos,
            )
            .map_err(|error| ScannerError::Logical(error.to_string()))?;
            last_publish_height = *applied_height;
        }

        Ok(())
    })
    .await
    .map_err(|error| {
        ScannerIterationError::Step(StepError::LogicalError {
            message: format!("wallet scanner block stream failed: {error}"),
        })
    })?;

    if let Some(error) = continuity_error {
        return Err(ScannerIterationError::Continuity(error));
    }

    if !connected_to_tip && streamed_block_count > 0 {
        return Err(ScannerIterationError::Continuity(
            wallet_scanner_no_connected_block_error(
                config,
                initial_applied_tip,
                streamed_block_count,
                from_slot,
                best_node.slot,
            ),
        ));
    }

    if applied_block_count == 0 {
        if best_node.height > *applied_height || best_node.slot > applied_slot.unwrap_or_default() {
            return Err(ScannerIterationError::Step(StepError::LogicalError {
                message: format!(
                    "wallet scanner source '{}' advanced to height {} slot {} but streamed no \
                    applicable blocks from slot {}",
                    best_node.node_name, best_node.height, best_node.slot, from_slot
                ),
            }));
        }

        if !published_genesis
            && Some(best_node.tip.as_str())
                == applied_tip.as_ref().map(ToString::to_string).as_deref()
        {
            publish_wallet_state(
                &config.wallets,
                &best_node.same_tip_nodes,
                *applied_height,
                best_node.tip.clone(),
                accounting.wallet_utxos(),
            )?;
        }
    }

    publish_new_observed_transaction_hashes(config, accounting, &observed_tx_hashes_before);

    update_group_state(&config.scanner_state, &config.group_id, |group| {
        group.applied_height = *applied_height;
        group.applied_slot = *applied_slot;
        group.applied_tip = *applied_tip;
        group.observed_tx_hashes = accounting.observed_transaction_hashes().len();
        group.wallet_count = accounting.tracked_wallet_count();
        group.status = ScannerStatus::Tailing;
        group.last_error = None;
    });

    if applied_block_count > 0 {
        let observed_tx_count_after = accounting.observed_transaction_hashes().len();
        let newly_observed_tx_count =
            observed_tx_count_after.saturating_sub(observed_tx_count_before);

        info!(
            target: TARGET,
            "wallet scanner group '{}' at height {}: streamed_blocks={}, applied_blocks={}, newly_observed_txs={}",
            display_group_key(&config.group_id),
            *applied_height,
            streamed_block_count,
            applied_block_count,
            newly_observed_tx_count,
        );
    }

    Ok(())
}

fn publish_new_observed_transaction_hashes(
    config: &ForkGroupScannerConfig,
    accounting: &ScannerAccounting,
    observed_tx_hashes_before: &BTreeSet<TxHash>,
) {
    let mut observed = config
        .observed_transaction_hashes
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    observed.extend(
        accounting
            .observed_transaction_hashes()
            .difference(observed_tx_hashes_before)
            .copied(),
    );
}

fn publish_genesis_wallet_state_if_needed(
    config: &ForkGroupScannerConfig,
    accounting: &ScannerAccounting,
    best_node: &BestGroupNode,
    applied_height: &mut u64,
    applied_tip: &mut Option<HeaderId>,
    applied_slot: &mut Option<u64>,
    checkpoints: &mut VecDeque<ScannerCheckpoint>,
) -> Result<bool, StepError> {
    if applied_tip.is_some() || best_node.height != 0 {
        return Ok(false);
    }

    let tip = parse_header_id(&best_node.tip).ok_or_else(|| StepError::LogicalError {
        message: format!(
            "wallet scanner selected invalid genesis tip '{}' for group '{}'",
            best_node.tip,
            display_group_key(&config.group_id),
        ),
    })?;

    *applied_height = 0;
    *applied_tip = Some(tip);
    *applied_slot = Some(best_node.slot);
    publish_wallet_state(
        &config.wallets,
        &best_node.same_tip_nodes,
        *applied_height,
        best_node.tip.clone(),
        accounting.wallet_utxos(),
    )?;
    push_checkpoint(
        checkpoints,
        ScannerCheckpoint::new(
            accounting,
            *applied_height,
            *applied_tip,
            *applied_slot,
            best_node.same_tip_nodes.clone(),
        ),
    );
    Ok(true)
}

async fn verify_snapshot_rescan(
    config: &ForkGroupScannerConfig,
    accounting: &mut ScannerAccounting,
    client: &NodeHttpClient,
    best_node: &BestGroupNode,
    rescan: &SnapshotRescan,
) -> Result<(), ScannerIterationError> {
    if best_node.slot < rescan.slot {
        return Err(ScannerIterationError::Step(StepError::LogicalError {
            message: format!(
                "wallet scanner source '{}' for group '{}' is behind snapshot seed slot {}; \
                 source slot={}",
                best_node.node_name,
                display_group_key(&config.group_id),
                rescan.slot,
                best_node.slot,
            ),
        }));
    }

    let slot_from = rescan.slot.saturating_sub(rescan.blocks);
    let mut found_seed_tip = rescan.slot == 0;
    let streamed_block_count = stream_blocks_range(client, slot_from, rescan.slot, |block| {
        accounting.observe_block_transactions(&block);
        found_seed_tip |= block.header.id == rescan.tip;
        Ok(())
    })
    .await
    .map_err(|error| {
        ScannerIterationError::Step(StepError::LogicalError {
            message: format!("wallet scanner snapshot trailing rescan failed: {error}"),
        })
    })?;

    if !found_seed_tip {
        return Err(ScannerIterationError::Step(StepError::LogicalError {
            message: format!(
                "wallet scanner snapshot trailing rescan for group '{}' did not find seed tip {} \
                 in slot range {}..={}",
                display_group_key(&config.group_id),
                rescan.tip,
                slot_from,
                rescan.slot,
            ),
        }));
    }

    info!(
        target: TARGET,
        "wallet scanner group '{}' verified snapshot seed tip {} with trailing rescan: \
         streamed_blocks={}, slot_range={}..={}",
        display_group_key(&config.group_id),
        rescan.tip,
        streamed_block_count,
        slot_from,
        rescan.slot,
    );

    Ok(())
}

fn push_checkpoint(checkpoints: &mut VecDeque<ScannerCheckpoint>, checkpoint: ScannerCheckpoint) {
    let applied_height = checkpoint.applied_height;
    checkpoints.push_back(checkpoint);
    while checkpoints.len() > scanner_checkpoints_len(applied_height) {
        checkpoints.pop_front();
    }
}

/// Return the per-group checkpoint window size for the current applied height.
///
/// The scanner keeps a bounded history of `ScannerCheckpoint`s used for
/// continuity rollback. The window grows gradually with chain depth, but is
/// clamped to `[MIN_SCANNER_CHECKPOINTS, MAX_SCANNER_CHECKPOINTS]`.
///
/// Current rule:
/// - `applied_height / MAX_SCANNER_CHECKPOINTS + 1` (integer division),
/// - then clamped to the configured min/max bounds.
fn scanner_checkpoints_len(applied_height: u64) -> usize {
    let checkpoints = (applied_height / MAX_SCANNER_CHECKPOINTS as u64) as usize + 1;
    checkpoints.clamp(MIN_SCANNER_CHECKPOINTS, MAX_SCANNER_CHECKPOINTS)
}

/// Select a rollback checkpoint after a continuity failure.
///
/// Prefers the newest checkpoint at or below
/// `applied_height - SCANNER_ROLLBACK_BLOCKS`. If no such checkpoint exists,
/// falls back to the oldest retained checkpoint (`front()`), which still allows
/// progress toward recovery.
///
/// Returns `None` only when the checkpoint deque is empty.
fn rollback_checkpoint(
    checkpoints: &VecDeque<ScannerCheckpoint>,
    applied_height: u64,
) -> Option<ScannerCheckpoint> {
    let target_height = applied_height.saturating_sub(SCANNER_ROLLBACK_BLOCKS);
    checkpoints
        .iter()
        .rev()
        .find(|checkpoint| checkpoint.applied_height <= target_height)
        .or_else(|| checkpoints.front())
        .cloned()
}

fn scanner_iteration_error_message(error: &ScannerIterationError) -> String {
    match error {
        ScannerIterationError::Step(error) | ScannerIterationError::Continuity(error) => {
            error.to_string()
        }
    }
}

fn wallet_scanner_continuity_error(
    config: &ForkGroupScannerConfig,
    streamed_parent: HeaderId,
    expected_parent: HeaderId,
    skipped_unconnected_block_count: usize,
) -> StepError {
    StepError::LogicalError {
        message: format!(
            "wallet scanner chain continuity failed for group '{}': streamed block parent {} does \
            not match applied tip {}; skipped_unconnected_blocks={}",
            display_group_key(&config.group_id),
            streamed_parent,
            expected_parent,
            skipped_unconnected_block_count,
        ),
    }
}

fn wallet_scanner_no_connected_block_error(
    config: &ForkGroupScannerConfig,
    applied_tip: Option<HeaderId>,
    streamed_block_count: usize,
    from_slot: u64,
    to_slot: u64,
) -> StepError {
    StepError::LogicalError {
        message: format!(
            "wallet scanner chain continuity failed for group '{}': no streamed block connected to \
            applied tip {:?}; streamed_blocks={}, slot_range={}..={}",
            display_group_key(&config.group_id),
            applied_tip,
            streamed_block_count,
            from_slot,
            to_slot,
        ),
    }
}

fn publish_wallet_state(
    wallets: &Arc<Mutex<TrackedWallets>>,
    source_node_names: &[String],
    height: u64,
    header_id: String,
    wallet_utxos: impl IntoIterator<Item = (WalletId, Vec<lb_core::mantle::Utxo>)>,
) -> Result<(), StepError> {
    let wallet_utxos = wallet_utxos.into_iter().collect::<Vec<_>>();
    let mut wallets = wallets.lock().map_err(|_| StepError::LogicalError {
        message: "wallet scanner wallet state lock is poisoned".to_owned(),
    })?;
    for source_node_name in source_node_names {
        wallets.record_header_height(source_node_name, &header_id, height);
    }
    wallets.record_observed_wallets_utxos(header_id, wallet_utxos.iter().cloned());
    wallets.replace_current_wallets_utxos(wallet_utxos);
    drop(wallets);
    Ok(())
}

fn update_group_state(
    state: &SharedWalletScannerState,
    group_id: &str,
    update: impl FnOnce(&mut ForkGroupScannerState),
) {
    if let Some(group) = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .groups
        .get_mut(group_id)
    {
        update(group);
    }
}

fn parse_header_id(value: &str) -> Option<HeaderId> {
    let value = value.trim().trim_start_matches("0x");
    let Ok(bytes) = hex::decode(value) else {
        return None;
    };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes.as_slice()) else {
        return None;
    };
    Some(HeaderId::from(bytes))
}
