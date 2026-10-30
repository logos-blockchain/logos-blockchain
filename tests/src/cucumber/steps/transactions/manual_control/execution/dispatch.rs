use std::{collections::HashMap, sync::Arc, time::Instant};

use lb_core::{header::HeaderId, mantle::Utxo};

use super::{
    BTreeMap, BuildHasher, CucumberWorld, Duration, HashSet, ManualCommand, NoteId, StepError,
    TARGET, TxHash, WalletOutputState, WalletSendReadiness, WalletUtxos, all_user_wallets,
    build_cycle_fee_policy, clear_all_wallet_encumbrances, clear_wallet_encumbrances,
    create_snapshot_all_nodes_with_wallet_state, create_snapshot_node_with_wallet_state,
    current_available_utxos_for_user_wallets,
    dependent::{DependentBurstDiagnostics, DependentTransactionLoadState, shuffle_if_dependent},
    drain_all_node_wallets, execute_coin_split, execute_coin_split_with_utxo_cache,
    execute_continuous_round_robin, execute_drain, execute_send, export_funds, extend_note_id_set,
    extend_tx_hash_set, handle_verify_command, info, log_wallet_balance, log_wallet_balances,
    nodes, prepare_ring_send_round_send_with_utxo_cache, request_faucet_funds_all_funding_wallets,
    request_faucet_funds_all_user_wallets, restart_node, sync, utils,
    validate_fee_horizon_after_wallet_batch, verify_no_duplicate_transactions,
    wait_for_all_nodes_to_be_synced_to_chain, wait_for_observed_transaction_hashes,
    wait_for_observed_transaction_hashes_cancellable,
};
use crate::{
    common::wallet::scanner::state::{ScannerBlockObservation, SharedWalletScannerState},
    cucumber::{
        steps::{
            mempool::steps::record_mempool_pending_counts,
            nodes::diagnostics::BlendDiagnosticEventLogger,
        },
        wallet::submissions::SignedUserWalletSubmissionNetworkResult,
    },
};

pub async fn execute_manual_command(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<bool, StepError> {
    if matches!(command, ManualCommand::Stop) {
        return Ok(true);
    }

    execute_non_stop_manual_command(world, step, command).await?;
    Ok(false)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Cucumber transaction shape and fee policy inputs"
)]
pub async fn execute_continuous_round_robin_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    coin_split_outputs: usize,
    coin_split_value: u64,
    num_transactions: usize,
    value: u64,
    cycles: usize,
    epochs_headroom: u32,
) -> Result<(), StepError> {
    let command = ManualCommand::ContinuousRoundRobinUserWallets {
        coin_split_outputs,
        coin_split_value,
        num_transactions,
        value,
        cycles,
        epochs_headroom,
    };
    execute_non_stop_manual_command(world, step, &command).await
}

pub async fn execute_coin_splits_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    splits_per_wallet: usize,
    outputs: usize,
    value: u64,
) -> Result<(), StepError> {
    let mut wallet_names: Vec<_> = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect();
    if wallet_names.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "coin split for all user wallets requires at least two wallets".to_owned(),
        });
    }
    wallet_names.sort();
    let mut available_utxos = current_available_utxos_for_user_wallets(world, step).await?;

    for wallet_name in &wallet_names {
        let best_node_info = sync::wait_wallet_send_ready(
            world,
            step,
            wallet_name,
            180,
            splits_per_wallet as u64 * outputs as u64 * value,
            WalletSendReadiness::TotalValueOnly,
            &mut available_utxos,
            &HashSet::new(),
        )
        .await?;

        for _ in 0..splits_per_wallet {
            execute_coin_split_with_utxo_cache(
                world,
                step,
                wallet_name,
                outputs,
                value,
                Some(&best_node_info),
                &mut available_utxos,
            )
            .await?;
        }
    }

    Ok(())
}

pub async fn verify_min_outputs_all_user_wallets(
    world: &mut CucumberWorld,
    step: &str,
    min_outputs: usize,
    timeout_seconds: u64,
    wallet_state_type: WalletOutputState,
) -> Result<(), StepError> {
    let mut wallet_names: Vec<_> = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect();
    wallet_names.sort();

    for wallet_name in &wallet_names {
        utils::wait_for_wallet_output_state(
            world,
            step,
            wallet_name.clone(),
            Some(&min_outputs),
            None,
            None,
            None,
            timeout_seconds,
            wallet_state_type,
        )
        .await?;
    }

    Ok(())
}

fn destructure_next_wallet_command(
    command: &ManualCommand,
) -> Result<(usize, usize, u64, u32), StepError> {
    let ManualCommand::ContinuousNextWalletUserWallets {
        cycles,
        num_transactions,
        value,
        epochs_headroom,
    } = command
    else {
        return Err(StepError::LogicalError {
            message: "expected ContinuousNextWalletUserWallets command".to_owned(),
        });
    };
    Ok((*cycles, *num_transactions, *value, *epochs_headroom))
}

pub async fn execute_continuous_next_wallet_user_wallet(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        command,
        None,
        None,
        false,
        1,
        &mut ContinuousWorkloadReservations::default(),
    )
    .await
    .map(|_| ())
}

pub async fn execute_mempool_next_wallet_user_wallet(
    world: &mut CucumberWorld,
    step: &str,
    cycles: usize,
    transactions_per_wallet: usize,
    value: u64,
    epochs_headroom: u32,
    dependent_mode: bool,
) -> Result<(), StepError> {
    if cycles == 0 || transactions_per_wallet == 0 {
        return Err(StepError::InvalidArgument {
            message:
                "mempool diagnostic rounds and transactions per wallet must be greater than zero"
                    .to_owned(),
        });
    }
    let command = ManualCommand::ContinuousNextWalletUserWallets {
        cycles,
        num_transactions: transactions_per_wallet,
        value,
        epochs_headroom,
    };
    let dependent_state = dependent_mode.then(DependentTransactionLoadState::new);
    let wallet_count = all_user_wallets(world)?.len();
    let transactions_per_round = wallet_count
        .checked_mul(transactions_per_wallet)
        .ok_or_else(|| StepError::InvalidArgument {
            message: "mempool diagnostic transaction count overflows usize".to_owned(),
        })?;
    let total_transactions =
        transactions_per_round
            .checked_mul(cycles)
            .ok_or_else(|| StepError::InvalidArgument {
                message: "mempool diagnostic total transaction count overflows usize".to_owned(),
            })?;
    let workload_mode = if dependent_mode {
        "dependent"
    } else {
        "independent"
    };
    let logger = BlendDiagnosticEventLogger::from_world(world);
    logger.append_named_timeline_record(
        "mempool_transaction_load_started",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "channel_id": dependent_state
                .as_ref()
                .map(|state| state.channel_id().to_string()),
            "wallet_count": wallet_count,
            "transactions_per_wallet": transactions_per_wallet,
            "transactions_per_round": transactions_per_round,
            "rounds": cycles,
            "total_transactions": total_transactions,
            "value": value,
            "epochs_headroom": epochs_headroom,
            "initial_shuffle_seed": dependent_mode.then_some(42),
        }),
    );

    let started = Instant::now();
    let _included_transaction_count = execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        &command,
        None,
        dependent_state.as_ref(),
        true,
        1,
        &mut ContinuousWorkloadReservations::default(),
    )
    .await?;
    let duration = started.elapsed();
    info!(
        target: TARGET,
        workload_mode,
        wallets = wallet_count,
        transactions_per_wallet,
        rounds = cycles,
        total_transactions,
        total_runtime_ms = duration.as_millis(),
        "Completed configured mempool transaction load"
    );
    logger.append_named_timeline_record(
        "mempool_transaction_load_completed",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "wallet_count": wallet_count,
            "transactions_per_wallet": transactions_per_wallet,
            "transactions_per_round": transactions_per_round,
            "rounds": cycles,
            "total_transactions": total_transactions,
            "total_runtime_ms": duration.as_millis(),
            "final_lineage_counter": dependent_state
                .as_ref()
                .and_then(DependentTransactionLoadState::last_burst_diagnostics)
                .and_then(|diagnostics| diagnostics.lineage_counter_end),
        }),
    );
    Ok(())
}

pub async fn execute_continuous_next_wallet_user_wallet_with_cancellation(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    workload_reservations: &mut ContinuousWorkloadReservations,
) -> Result<(usize, usize), StepError> {
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        command,
        Some(cancellation),
        None,
        false,
        1,
        workload_reservations,
    )
    .await
}

pub async fn execute_continuous_dependent_next_wallet_user_wallet_with_cancellation(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
    cancellation: &mut tokio::sync::watch::Receiver<bool>,
    dependent_state: &DependentTransactionLoadState,
    first_round: usize,
    workload_reservations: &mut ContinuousWorkloadReservations,
) -> Result<(usize, usize), StepError> {
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        command,
        Some(cancellation),
        Some(dependent_state),
        false,
        first_round,
        workload_reservations,
    )
    .await
}

struct NextWalletRoundCompletion {
    round_number: usize,
    transaction_count: usize,
    included_transaction_count: usize,
    outstanding_transaction_count: usize,
    completion_reason: &'static str,
    inclusion_verification_duration: Duration,
    total_round_duration: Duration,
    dependent_diagnostics: Option<DependentBurstDiagnostics>,
}

fn log_next_wallet_round_started(
    world: &CucumberWorld,
    mempool_diagnostics: bool,
    workload_mode: &str,
    round_number: usize,
    transaction_count: usize,
) {
    if !mempool_diagnostics {
        return;
    }
    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "mempool_transaction_round_started",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "round": round_number,
            "transaction_count": transaction_count,
        }),
    );
}

fn log_next_wallet_round_completed(
    world: &CucumberWorld,
    mempool_diagnostics: bool,
    workload_mode: &str,
    completion: &NextWalletRoundCompletion,
) {
    let dependent_diagnostics = completion.dependent_diagnostics.as_ref();
    info!(
        target: TARGET,
        workload_mode,
        round = completion.round_number,
        transaction_count = completion.transaction_count,
        included_transaction_count = completion.included_transaction_count,
        outstanding_transaction_count = completion.outstanding_transaction_count,
        completion_reason = completion.completion_reason,
        inclusion_verification_ms = completion.inclusion_verification_duration.as_millis(),
        total_round_ms = completion.total_round_duration.as_millis(),
        shuffle_seed = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.shuffle_seed),
        lineage_counter_start = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_start),
        lineage_counter_end = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_end),
        "Completed next-wallet transaction load round"
    );
    if !mempool_diagnostics {
        return;
    }
    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "mempool_transaction_round_completed",
        &serde_json::json!({
            "workload_mode": workload_mode,
            "round": completion.round_number,
            "transaction_count": completion.transaction_count,
            "included_transaction_count": completion.included_transaction_count,
            "outstanding_transaction_count": completion.outstanding_transaction_count,
            "completion_reason": completion.completion_reason,
            "inclusion_verification_ms": completion.inclusion_verification_duration.as_millis(),
            "total_round_ms": completion.total_round_duration.as_millis(),
            "shuffle_seed": dependent_diagnostics.and_then(|diagnostics| diagnostics.shuffle_seed),
            "lineage_counter_start": dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_start),
            "lineage_counter_end": dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_end),
        }),
    );
}

const fn next_wallet_mode(dependent: bool) -> (&'static str, &'static str) {
    if dependent {
        ("CONTINUOUS DEPENDENT NEXT WALLET", "dependent")
    } else {
        ("CONTINUOUS NEXT WALLET", "independent")
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Round completion keeps strict inclusion and block-driven workload waits explicit."
)]
async fn verify_next_wallet_round_completion(
    world: &mut CucumberWorld,
    step: &str,
    transaction_hashes: &HashSet<TxHash>,
    expected_transaction_count: usize,
    round: Option<usize>,
    mode: &str,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    scanner_group_id: Option<&str>,
    reservation_retirement: Option<ContinuousWorkloadRetirementContext<'_>>,
    workload_reservations: &mut ContinuousWorkloadReservations,
) -> Result<ContinuousRoundCompletion, StepError> {
    if let Some(group_id) = scanner_group_id {
        let scanner_state = Arc::clone(&world.scanner.state);
        if let Some(context) = reservation_retirement {
            let mut observe = || {
                maintain_continuous_workload_reservations(world, workload_reservations, context)
                    .map(|_| ())
            };
            return wait_for_continuous_round_completion_with_timeout(
                scanner_state,
                group_id,
                step,
                transaction_hashes,
                expected_transaction_count,
                round,
                cancellation,
                CONTINUOUS_ROUND_INCLUSION_TIMEOUT,
                Some(&mut observe),
            )
            .await;
        }
        return wait_for_continuous_round_completion(
            scanner_state,
            group_id,
            step,
            transaction_hashes,
            expected_transaction_count,
            round,
            cancellation,
        )
        .await;
    }

    verify_transactions_mined(
        world,
        step,
        transaction_hashes,
        expected_transaction_count,
        round,
        mode,
        "D",
        cancellation,
    )
    .await?;
    Ok(ContinuousRoundCompletion {
        included_transaction_hashes: transaction_hashes.clone(),
        outstanding_transaction_hashes: HashSet::new(),
        included_transaction_count: transaction_hashes.len(),
        outstanding_transaction_count: 0,
        reason: "all_transactions_included",
    })
}

fn verify_next_wallet_workload_complete(
    mode: &str,
    cycles: usize,
    wallet_count: usize,
    transactions_per_wallet: usize,
    unique_hash_count: usize,
) -> Result<(), StepError> {
    let expected_total = cycles * wallet_count * transactions_per_wallet;
    if unique_hash_count != expected_total {
        return Err(StepError::StepFail {
            message: format!(
                "{mode} submitted {unique_hash_count} unique transaction hash(es), expected \
                {expected_total}",
            ),
        });
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "A continuous wallet round keeps submission, chain wait, diagnostics, and reservation state ordered."
)]
#[expect(
    clippy::too_many_arguments,
    reason = "The background workload owns reservation tracking across executor calls."
)]
async fn execute_continuous_next_wallet_user_wallet_inner(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    dependent_state: Option<&DependentTransactionLoadState>,
    mempool_diagnostics: bool,
    first_round: usize,
    workload_reservations: &mut ContinuousWorkloadReservations,
) -> Result<(usize, usize), StepError> {
    let (cycles, transactions_per_wallet, value, epochs_headroom) =
        destructure_next_wallet_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let (mut used_input_note_ids, mut all_next_wallet_tx_hashes) = (HashSet::new(), HashSet::new());
    let (mut included_transaction_count, mut outstanding_transaction_count) = (0usize, 0usize);
    let scanner_group_id = if cancellation.is_some() && !mempool_diagnostics {
        Some(scanner_group_for_wallets(world, &wallet_names)?)
    } else {
        None
    };
    let reservation_retirement = match scanner_group_id.as_deref() {
        Some(scanner_group_id) => {
            let security_parameter = world
                .startup
                .manual_node_config_overrides
                .cryptarchia_security_param
                .ok_or_else(|| StepError::LogicalError {
                    message: "continuous Blend workload has no effective Cryptarchia security parameter for reservation aging".to_owned(),
                })?
                .get();
            Some(ContinuousWorkloadRetirementContext {
                scanner_group_id,
                security_parameter,
            })
        }
        None => None,
    };
    let (mode, workload_mode) = next_wallet_mode(dependent_state.is_some());
    for cycle in 0..cycles {
        let round_started = Instant::now();
        let round_number = first_round.saturating_add(cycle);
        let displayed_round = if dependent_state.is_some() {
            round_number
        } else {
            cycle + 1
        };
        let cycle_index_for_logs = displayed_round.saturating_sub(1);
        if let Some(dependent_state) = dependent_state {
            dependent_state.clear_last_burst_diagnostics();
        }
        if scanner_group_id.is_some() {
            // The scanner's tracked reservations already exclude pending inputs.
            // Keep this set round-local so mined or explicitly retired UTXOs can
            // be selected again once they are available on the canonical chain.
            used_input_note_ids.clear();
        }
        log_next_wallet_round_started(
            world,
            mempool_diagnostics,
            workload_mode,
            round_number,
            wallet_names.len() * transactions_per_wallet,
        );
        if let Some(context) = reservation_retirement {
            let _summary =
                maintain_continuous_workload_reservations(world, workload_reservations, context)?;
        }
        let mut available_utxos = current_available_utxos_for_user_wallets(world, step).await?;

        let (cycle_tx_hashes, cycle_used_input_note_ids) = execute_ring_send_round_with_utxo_cache(
            world,
            step,
            &wallet_names,
            transactions_per_wallet,
            value,
            cycle,
            round_number,
            epochs_headroom,
            &mut available_utxos,
            &used_input_note_ids,
            dependent_state,
            mempool_diagnostics,
            reservation_retirement,
            workload_reservations,
        )
        .await?;
        verify_no_duplicate_transactions(
            &cycle_tx_hashes,
            &all_next_wallet_tx_hashes,
            cycle_index_for_logs,
            mode,
        )?;
        extend_note_id_set(&mut used_input_note_ids, &cycle_used_input_note_ids);

        let verification_started = Instant::now();
        let round_completion = verify_next_wallet_round_completion(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * transactions_per_wallet,
            Some(displayed_round),
            mode,
            cancellation.as_deref_mut(),
            scanner_group_id.as_deref(),
            reservation_retirement,
            workload_reservations,
        )
        .await?;
        let verification_duration = verification_started.elapsed();
        if let Some(context) = reservation_retirement {
            workload_reservations.finish_round(&round_completion);
            let retirement_summary =
                maintain_continuous_workload_reservations(world, workload_reservations, context)?;
            log_continuous_workload_round_diagnostics(
                world,
                step,
                &wallet_names,
                round_number,
                &round_completion,
                context.security_parameter,
                workload_reservations,
                retirement_summary,
            )
            .await?;
        }
        included_transaction_count =
            included_transaction_count.saturating_add(round_completion.included_transaction_count);
        outstanding_transaction_count = outstanding_transaction_count
            .saturating_add(round_completion.outstanding_transaction_count);
        if mempool_diagnostics {
            record_mempool_pending_counts(
                world,
                workload_mode,
                &format!("round_{round_number}_included"),
            )
            .await?;
        }
        log_next_wallet_round_completed(
            world,
            mempool_diagnostics,
            workload_mode,
            &NextWalletRoundCompletion {
                round_number,
                transaction_count: cycle_tx_hashes.len(),
                included_transaction_count: round_completion.included_transaction_count,
                outstanding_transaction_count: round_completion.outstanding_transaction_count,
                completion_reason: round_completion.reason,
                inclusion_verification_duration: verification_duration,
                total_round_duration: round_started.elapsed(),
                dependent_diagnostics: dependent_state
                    .and_then(DependentTransactionLoadState::last_burst_diagnostics),
            },
        );
        extend_tx_hash_set(&mut all_next_wallet_tx_hashes, &cycle_tx_hashes);
    }

    verify_next_wallet_workload_complete(
        mode,
        cycles,
        wallet_names.len(),
        transactions_per_wallet,
        all_next_wallet_tx_hashes.len(),
    )?;

    info!(
        target: TARGET,
        "{mode} scenario complete: {} unique transaction(s) submitted \
        across {} cycle(s)",
        all_next_wallet_tx_hashes.len(),
        cycles,
    );

    if let Some(context) = reservation_retirement {
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "continuous_workload_completed",
            &serde_json::json!({
                "rounds": cycles,
                "submitted_transactions": all_next_wallet_tx_hashes.len(),
                "included_transactions_at_round_checkpoints": included_transaction_count,
                "outstanding_transactions_at_round_checkpoints": outstanding_transaction_count,
                "security_parameter": context.security_parameter,
                "retired_reservations": workload_reservations.retired.len(),
                "retired_wallet_count": workload_reservations.retired_wallets.len(),
                "retired_inputs": workload_reservations.retired_input_count,
                "retired_input_value": workload_reservations.retired_input_value,
                "latest_retired_height": workload_reservations.latest_retired_height,
                "outstanding_reservation_count": workload_reservations.active.len(),
                "outstanding_reserved_input_count": workload_reservations
                    .active
                    .values()
                    .map(|reservation| reservation.inputs.len())
                    .sum::<usize>(),
                "outstanding_reserved_input_value": workload_reservations
                    .active
                    .values()
                    .flat_map(|reservation| &reservation.inputs)
                    .map(|input| input.note.value)
                    .sum::<u64>(),
            }),
        );
    }

    Ok((included_transaction_count, outstanding_transaction_count))
}

const MAX_BLOCK_TRANSACTIONS: usize = 1_024;
const OPEN_BLOCK_MAX_TRANSACTIONS: usize = MAX_BLOCK_TRANSACTIONS * 95 / 100;
const CONTINUOUS_ROUND_INCLUSION_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Debug, Clone, Eq, PartialEq)]
struct ContinuousRoundCompletion {
    included_transaction_hashes: HashSet<TxHash>,
    outstanding_transaction_hashes: HashSet<TxHash>,
    included_transaction_count: usize,
    outstanding_transaction_count: usize,
    reason: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ContinuousWorkloadRetirementContext<'a> {
    scanner_group_id: &'a str,
    security_parameter: u32,
}

#[derive(Debug, Clone)]
struct ContinuousWorkloadReservation {
    wallet_name: String,
    inputs: Vec<Utxo>,
    reserved_height: u64,
    reserved_tip: Option<HeaderId>,
    eligible_for_retirement: bool,
}

#[derive(Debug, Default)]
/// Reservation and late-inclusion records owned by the continuous workload
/// task.
pub struct ContinuousWorkloadReservations {
    active: HashMap<TxHash, ContinuousWorkloadReservation>,
    retired: HashSet<TxHash>,
    reported_late_inclusions: HashSet<TxHash>,
    retired_input_count: usize,
    retired_input_value: u64,
    latest_retired_height: u64,
    retired_wallets: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ReservationRetirementCandidate {
    tx_hash: TxHash,
    wallet_name: String,
    inputs: Vec<Utxo>,
}

#[derive(Debug, Default)]
struct ContinuousReservationMaintenanceSummary {
    included_reservation_count: usize,
    eligible_reservation_count: usize,
    eligible_input_count: usize,
    eligible_input_value: u64,
    age_eligible_reservation_count: usize,
    retired_reservation_count: usize,
    retired_input_count: usize,
    retired_input_value: u64,
    reorg_reanchored_reservation_count: usize,
    reconciled_reservation_count: usize,
    late_inclusion_tx_hashes: Vec<TxHash>,
}

impl ContinuousWorkloadReservations {
    fn register(
        &mut self,
        tx_hash: TxHash,
        wallet_name: String,
        inputs: Vec<Utxo>,
        reserved_height: u64,
        reserved_tip: Option<HeaderId>,
    ) {
        self.active.insert(
            tx_hash,
            ContinuousWorkloadReservation {
                wallet_name,
                inputs,
                reserved_height,
                reserved_tip,
                eligible_for_retirement: false,
            },
        );
    }

    fn finish_round(&mut self, completion: &ContinuousRoundCompletion) {
        for tx_hash in &completion.included_transaction_hashes {
            self.active.remove(tx_hash);
        }
        for tx_hash in &completion.outstanding_transaction_hashes {
            if let Some(reservation) = self.active.get_mut(tx_hash) {
                reservation.eligible_for_retirement = true;
            }
        }
    }

    fn tracked_tx_hashes(&self) -> HashSet<TxHash> {
        self.active
            .keys()
            .chain(self.retired.iter())
            .copied()
            .collect()
    }

    fn observe_canonical_chain(
        &mut self,
        current_height: u64,
        current_tip: Option<HeaderId>,
        canonical_block_ids: &HashMap<u64, HeaderId>,
        observed_workload_tx_hashes: &HashSet<TxHash>,
        security_parameter: u32,
    ) -> (
        ContinuousReservationMaintenanceSummary,
        Vec<ReservationRetirementCandidate>,
    ) {
        let mut summary = ContinuousReservationMaintenanceSummary::default();
        for tx_hash in observed_workload_tx_hashes {
            if self.active.remove(tx_hash).is_some() {
                summary.included_reservation_count += 1;
            }
            if self.retired.contains(tx_hash) && self.reported_late_inclusions.insert(*tx_hash) {
                summary.late_inclusion_tx_hashes.push(*tx_hash);
            }
        }

        for reservation in self.active.values_mut() {
            let anchor_is_canonical = reservation.reserved_height == 0
                || canonical_block_ids
                    .get(&reservation.reserved_height)
                    .is_some_and(|block_id| Some(*block_id) == reservation.reserved_tip);
            if current_height < reservation.reserved_height || !anchor_is_canonical {
                reservation.reserved_height = current_height;
                reservation.reserved_tip = current_tip;
                summary.reorg_reanchored_reservation_count += 1;
            }
        }

        let active_note_ids = self
            .active
            .iter()
            .map(|(tx_hash, reservation)| {
                (
                    *tx_hash,
                    reservation
                        .inputs
                        .iter()
                        .map(Utxo::id)
                        .collect::<HashSet<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
        summary.eligible_reservation_count = self
            .active
            .values()
            .filter(|reservation| reservation.eligible_for_retirement)
            .count();
        for reservation in self
            .active
            .values()
            .filter(|reservation| reservation.eligible_for_retirement)
        {
            summary.eligible_input_count += reservation.inputs.len();
            summary.eligible_input_value += reservation
                .inputs
                .iter()
                .map(|utxo| utxo.note.value)
                .sum::<u64>();
        }

        let mut candidates = Vec::new();
        for (tx_hash, reservation) in &self.active {
            if !reservation.eligible_for_retirement
                || current_height.saturating_sub(reservation.reserved_height)
                    < u64::from(security_parameter)
            {
                continue;
            }
            summary.age_eligible_reservation_count += 1;
            let inputs = reservation
                .inputs
                .iter()
                .filter(|input| {
                    !active_note_ids.iter().any(|(other_hash, note_ids)| {
                        other_hash != tx_hash && note_ids.contains(&input.id())
                    })
                })
                .copied()
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                continue;
            }
            candidates.push(ReservationRetirementCandidate {
                tx_hash: *tx_hash,
                wallet_name: reservation.wallet_name.clone(),
                inputs,
            });
        }

        (summary, candidates)
    }

    fn mark_retired(
        &mut self,
        candidate: &ReservationRetirementCandidate,
        released_inputs: &[Utxo],
        current_height: u64,
    ) {
        self.active.remove(&candidate.tx_hash);
        self.retired.insert(candidate.tx_hash);
        self.retired_wallets.insert(candidate.wallet_name.clone());
        self.retired_input_count += released_inputs.len();
        self.retired_input_value += released_inputs
            .iter()
            .map(|utxo| utxo.note.value)
            .sum::<u64>();
        self.latest_retired_height = self.latest_retired_height.max(current_height);
    }

    fn resolve_without_retirement(&mut self, tx_hash: TxHash) {
        self.active.remove(&tx_hash);
    }
}

fn scanner_group_for_wallets(
    world: &CucumberWorld,
    wallet_names: &[String],
) -> Result<String, StepError> {
    let mut selected_group: Option<String> = None;
    for wallet_name in wallet_names {
        let wallet = world
            .wallet_registry
            .wallet_info
            .get(wallet_name)
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "continuous transaction workload wallet '{wallet_name}' has no scanner metadata"
                ),
            })?;
        let group_id = if world.fork_groups.groups().is_empty() {
            world
                .fork_groups
                .mapping()
                .get(&wallet.node_name)
                .cloned()
                .unwrap_or_default()
        } else {
            world
                .fork_groups
                .mapping()
                .get(&wallet.node_name)
                .cloned()
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "continuous transaction workload wallet '{}' is not assigned to a scanner group",
                        wallet.wallet_name
                    ),
                })?
        };
        if selected_group
            .as_ref()
            .is_some_and(|selected| selected != &group_id)
        {
            return Err(StepError::LogicalError {
                message: "continuous transaction workload wallets span multiple scanner groups; \
                          block-driven round completion requires one selected canonical chain"
                    .to_owned(),
            });
        }
        selected_group = Some(group_id);
    }
    selected_group.ok_or_else(|| StepError::InvalidArgument {
        message: "continuous transaction workload requires at least one wallet".to_owned(),
    })
}

fn canonical_scanner_anchor(
    world: &CucumberWorld,
    scanner_group_id: &str,
) -> Result<(u64, Option<HeaderId>), StepError> {
    let scanner_state = world
        .scanner
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let group =
        scanner_state
            .groups
            .get(scanner_group_id)
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "continuous workload scanner group '{scanner_group_id}' has not been published"
                ),
            })?;
    let anchor = (group.applied_height, group.applied_tip);
    drop(scanner_state);
    Ok(anchor)
}

fn evaluate_continuous_round_completion(
    expected_transaction_hashes: &HashSet<TxHash>,
    blocks: &[ScannerBlockObservation],
) -> Option<ContinuousRoundCompletion> {
    let mut included_transaction_hashes = HashSet::with_capacity(expected_transaction_hashes.len());
    let mut latest_inclusion_height = None;
    let inclusion_threshold = expected_transaction_hashes.len().div_ceil(2);
    let mut inclusion_threshold_reached = false;
    let mut qualifying_open_block_count = 0usize;

    for block in blocks {
        let mut newly_included_count = 0usize;
        for transaction_hash in &block.transaction_hashes {
            if expected_transaction_hashes.contains(transaction_hash)
                && included_transaction_hashes.insert(*transaction_hash)
            {
                newly_included_count += 1;
            }
        }

        if newly_included_count > 0 {
            latest_inclusion_height = Some(block.height);
            if included_transaction_hashes.len() == expected_transaction_hashes.len() {
                return Some(continuous_round_completion(
                    expected_transaction_hashes,
                    included_transaction_hashes,
                    "all_transactions_included",
                ));
            }
            if included_transaction_hashes.len() >= inclusion_threshold {
                inclusion_threshold_reached = true;
                qualifying_open_block_count = 0;
            }
        } else if inclusion_threshold_reached
            && latest_inclusion_height.is_some_and(|height| block.height > height)
            && block.transaction_count <= OPEN_BLOCK_MAX_TRANSACTIONS
        {
            qualifying_open_block_count += 1;
            if qualifying_open_block_count == 3 {
                return Some(continuous_round_completion(
                    expected_transaction_hashes,
                    included_transaction_hashes,
                    "three_subsequent_open_blocks",
                ));
            }
        }
    }

    None
}

fn continuous_round_completion(
    expected_transaction_hashes: &HashSet<TxHash>,
    included_transaction_hashes: HashSet<TxHash>,
    reason: &'static str,
) -> ContinuousRoundCompletion {
    let outstanding_transaction_hashes = expected_transaction_hashes
        .difference(&included_transaction_hashes)
        .copied()
        .collect::<HashSet<_>>();
    ContinuousRoundCompletion {
        included_transaction_count: included_transaction_hashes.len(),
        outstanding_transaction_count: outstanding_transaction_hashes.len(),
        included_transaction_hashes,
        outstanding_transaction_hashes,
        reason,
    }
}

fn count_included_transaction_hashes(
    expected_transaction_hashes: &HashSet<TxHash>,
    blocks: &[ScannerBlockObservation],
) -> usize {
    blocks
        .iter()
        .flat_map(|block| &block.transaction_hashes)
        .filter(|transaction_hash| expected_transaction_hashes.contains(transaction_hash))
        .copied()
        .collect::<HashSet<_>>()
        .len()
}

async fn wait_for_continuous_round_completion(
    scanner_state: SharedWalletScannerState,
    group_id: &str,
    step: &str,
    transaction_hashes: &HashSet<TxHash>,
    expected_transaction_count: usize,
    round: Option<usize>,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<ContinuousRoundCompletion, StepError> {
    wait_for_continuous_round_completion_with_timeout(
        scanner_state,
        group_id,
        step,
        transaction_hashes,
        expected_transaction_count,
        round,
        cancellation,
        CONTINUOUS_ROUND_INCLUSION_TIMEOUT,
        None,
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "The wait loop keeps scanner, round, cancellation, and workload maintenance inputs explicit"
)]
async fn wait_for_continuous_round_completion_with_timeout(
    scanner_state: SharedWalletScannerState,
    group_id: &str,
    step: &str,
    transaction_hashes: &HashSet<TxHash>,
    expected_transaction_count: usize,
    round: Option<usize>,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    inclusion_timeout: Duration,
    mut workload_observer: Option<&mut (dyn FnMut() -> Result<(), StepError> + Send)>,
) -> Result<ContinuousRoundCompletion, StepError> {
    if transaction_hashes.len() != expected_transaction_count {
        return Err(StepError::StepFail {
            message: format!(
                "continuous workload{} submitted {} transaction hash(es), expected {expected_transaction_count}",
                round.map_or_else(String::new, |round| format!(" round {round}")),
                transaction_hashes.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "Step `{step}` waiting for full inclusion or at least 50% inclusion followed by three subsequent open blocks ({} submitted)",
        transaction_hashes.len(),
    );
    let started = Instant::now();
    loop {
        let (completion, scanner_status) = {
            let state = scanner_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.groups.get(group_id).map_or_else(
                || (None, "scanner group has not been published".to_owned()),
                |group| {
                    (
                        evaluate_continuous_round_completion(
                            transaction_hashes,
                            &group.recent_blocks,
                        ),
                        format!(
                            "source={:?} applied={} target={:?} status={:?} last_error={:?}",
                            group.source_node,
                            group.applied_height,
                            group.target_height,
                            group.status,
                            group.last_error
                        ),
                    )
                },
            )
        };
        if let Some(observe) = workload_observer.as_mut() {
            observe()?;
        }
        if let Some(completion) = completion {
            info!(
                target: TARGET,
                round = ?round,
                submitted_transactions = transaction_hashes.len(),
                included_transactions = completion.included_transaction_count,
                outstanding_transactions = completion.outstanding_transaction_count,
                completion_reason = completion.reason,
                "Completed continuous transaction round"
            );
            return Ok(completion);
        }

        if cancellation
            .as_ref()
            .is_some_and(|receiver| *receiver.borrow())
        {
            return Err(StepError::BackgroundTaskCancelled);
        }

        if started.elapsed() >= inclusion_timeout {
            let included = {
                let state = scanner_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.groups.get(group_id).map_or(0, |group| {
                    count_included_transaction_hashes(transaction_hashes, &group.recent_blocks)
                })
            };
            let outstanding = transaction_hashes.len().saturating_sub(included);
            return Err(StepError::Timeout {
                message: format!(
                    "Step `{step}` continuous transaction round inclusion timeout: submitted={} \
                    included={included} outstanding={outstanding} scanner_group='{}' \
                    scanner={scanner_status}",
                    transaction_hashes.len(),
                    if group_id.is_empty() {
                        "<ungrouped>"
                    } else {
                        group_id
                    },
                ),
            });
        }

        if let Some(receiver) = cancellation.as_deref_mut() {
            tokio::select! {
                changed = receiver.changed() => {
                    if changed.is_err() || *receiver.borrow() {
                        return Err(StepError::BackgroundTaskCancelled);
                    }
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        } else {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Reservation observation, selective release, and its diagnostic record form one maintenance pass"
)]
fn maintain_continuous_workload_reservations(
    world: &CucumberWorld,
    workload_reservations: &mut ContinuousWorkloadReservations,
    context: ContinuousWorkloadRetirementContext<'_>,
) -> Result<ContinuousReservationMaintenanceSummary, StepError> {
    let tracked_tx_hashes = workload_reservations.tracked_tx_hashes();
    let (current_height, current_tip, canonical_block_ids, observed_tx_hashes) = {
        let scanner_state = world
            .scanner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let group = scanner_state
            .groups
            .get(context.scanner_group_id)
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "continuous workload scanner group '{}' has not been published",
                    context.scanner_group_id
                ),
            })?;
        let snapshot = (
            group.applied_height,
            group.applied_tip,
            group
                .recent_blocks
                .iter()
                .map(|block| (block.height, block.block_id))
                .collect::<HashMap<_, _>>(),
            group
                .recent_blocks
                .iter()
                .flat_map(|block| block.transaction_hashes.iter().copied())
                .filter(|tx_hash| tracked_tx_hashes.contains(tx_hash))
                .collect::<HashSet<_>>(),
        );
        drop(scanner_state);
        snapshot
    };
    let (mut summary, candidates) = workload_reservations.observe_canonical_chain(
        current_height,
        current_tip,
        &canonical_block_ids,
        &observed_tx_hashes,
        context.security_parameter,
    );
    for candidate in candidates {
        let note_ids = candidate
            .inputs
            .iter()
            .map(Utxo::id)
            .collect::<HashSet<_>>();
        let released_inputs = world
            .with_wallets_mut(|wallets| {
                wallets.release_wallet_reserved_inputs_if_unspent(
                    &candidate.wallet_name,
                    &note_ids,
                )
            })
            .map_err(|error| StepError::StepFail {
                message: format!(
                    "continuous workload could not release reservation for transaction {:?}: {error}",
                    candidate.tx_hash
                ),
            })?;
        if released_inputs.is_empty() {
            workload_reservations.resolve_without_retirement(candidate.tx_hash);
            summary.reconciled_reservation_count += 1;
            continue;
        }
        workload_reservations.mark_retired(&candidate, &released_inputs, current_height);
        summary.retired_reservation_count += 1;
        summary.retired_input_count += released_inputs.len();
        summary.retired_input_value += released_inputs
            .iter()
            .map(|utxo| utxo.note.value)
            .sum::<u64>();
    }

    if summary.included_reservation_count > 0
        || summary.retired_reservation_count > 0
        || summary.reorg_reanchored_reservation_count > 0
        || summary.reconciled_reservation_count > 0
        || !summary.late_inclusion_tx_hashes.is_empty()
    {
        let retired_hashes = summary
            .late_inclusion_tx_hashes
            .iter()
            .map(|hash| format!("{hash:?}"))
            .collect::<Vec<_>>();
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "continuous_workload_reservation_maintenance",
            &serde_json::json!({
                "scanner_group": context.scanner_group_id,
                "scanner_height": current_height,
                "security_parameter": context.security_parameter,
                "included_reservations": summary.included_reservation_count,
                "eligible_reservations": summary.eligible_reservation_count,
                "eligible_inputs": summary.eligible_input_count,
                "eligible_input_value": summary.eligible_input_value,
                "age_eligible_reservations": summary.age_eligible_reservation_count,
                "retired_reservations": summary.retired_reservation_count,
                "retired_inputs": summary.retired_input_count,
                "retired_input_value": summary.retired_input_value,
                "reorg_reanchored_reservations": summary.reorg_reanchored_reservation_count,
                "reconciled_reservations": summary.reconciled_reservation_count,
                "late_inclusion_tx_hashes": retired_hashes,
            }),
        );
    }

    Ok(summary)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Round, wallet, security, and retirement diagnostics stay explicit at the event boundary"
)]
async fn log_continuous_workload_round_diagnostics(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    round: usize,
    completion: &ContinuousRoundCompletion,
    security_parameter: u32,
    workload_reservations: &ContinuousWorkloadReservations,
    retirement: ContinuousReservationMaintenanceSummary,
) -> Result<(), StepError> {
    let wallet_infos = wallet_names
        .iter()
        .map(|wallet_name| {
            world
                .wallet_registry
                .wallet_info
                .get(wallet_name)
                .cloned()
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "continuous workload wallet '{wallet_name}' has no tracked wallet metadata"
                    ),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let wallet_states =
        utils::current_wallet_states_for_wallets(world, step, &wallet_infos).await?;
    let wallet_balances = wallet_states
        .iter()
        .map(|(wallet_id, state)| {
            let on_chain = state.balance(WalletOutputState::OnChain);
            let reserved = state.balance(WalletOutputState::Reserved);
            let available = state.balance(WalletOutputState::Available);
            serde_json::json!({
                "wallet": wallet_id.as_str(),
                "on_chain_utxo_count": on_chain.output_count,
                "on_chain_value": on_chain.value,
                "reserved_utxo_count": reserved.output_count,
                "reserved_value": reserved.value,
                "available_utxo_count": available.output_count,
                "available_value": available.value,
            })
        })
        .collect::<Vec<_>>();
    let late_inclusion_hashes = retirement
        .late_inclusion_tx_hashes
        .iter()
        .map(|hash| format!("{hash:?}"))
        .collect::<Vec<_>>();

    BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
        "continuous_workload_round_completed",
        &serde_json::json!({
            "round": round,
            "submitted_transactions": completion.included_transaction_count
                + completion.outstanding_transaction_count,
            "included_transactions": completion.included_transaction_count,
            "outstanding_transactions": completion.outstanding_transaction_count,
            "completion_reason": completion.reason,
            "security_parameter": security_parameter,
            "eligible_reservations": retirement.eligible_reservation_count,
            "eligible_inputs": retirement.eligible_input_count,
            "eligible_input_value": retirement.eligible_input_value,
            "age_eligible_reservations": retirement.age_eligible_reservation_count,
            "retired_reservations_this_round": retirement.retired_reservation_count,
            "retired_inputs_this_round": retirement.retired_input_count,
            "retired_input_value_this_round": retirement.retired_input_value,
            "reorg_reanchored_reservations": retirement.reorg_reanchored_reservation_count,
            "reconciled_reservations": retirement.reconciled_reservation_count,
            "retired_reservation_total": workload_reservations.retired.len(),
            "retired_wallet_count": workload_reservations.retired_wallets.len(),
            "retired_input_total": workload_reservations.retired_input_count,
            "retired_input_value_total": workload_reservations.retired_input_value,
            "latest_retired_height": workload_reservations.latest_retired_height,
            "outstanding_reservation_count": workload_reservations.active.len(),
            "late_inclusion_tx_hashes": late_inclusion_hashes,
            "wallet_balances": wallet_balances,
        }),
    );

    if !retirement.late_inclusion_tx_hashes.is_empty() {
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "continuous_workload_late_inclusion_after_reservation_retirement",
            &serde_json::json!({
                "round": round,
                "tx_hashes": late_inclusion_hashes,
                "note": "retirement released test-wallet reservations; inclusion remains observable and may expose a conflict",
            }),
        );
    }

    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Transaction verification context and optional task cancellation stay explicit"
)]
pub(super) async fn verify_transactions_mined(
    world: &mut CucumberWorld,
    step: &str,
    tx_hashes: &HashSet<TxHash>,
    expected_tx_count: usize,
    cycle: Option<usize>,
    tag: &str,
    phase: &str,
    cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Result<(), StepError> {
    if tx_hashes.len() != expected_tx_count {
        return Err(StepError::StepFail {
            message: format!(
                "{tag}{} submitted {} transaction hash(es), expected {expected_tx_count}",
                cycle.map_or_else(String::new, |cycle| format!(" cycle {cycle}")),
                tx_hashes.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "{tag}{} {phase}: Wait for {} submitted transaction hashes to be observed in chain blocks",
        cycle.map_or_else(String::new, |cycle| format!(" cycle {cycle}")),
        tx_hashes.len(),
    );

    if let Some(cancellation) = cancellation {
        wait_for_observed_transaction_hashes_cancellable(
            world,
            step,
            tx_hashes,
            Duration::from_mins(10),
            cancellation,
        )
        .await
    } else {
        wait_for_observed_transaction_hashes(world, step, tx_hashes, Duration::from_mins(10)).await
    }
}

pub(super) fn log_phase_counts(
    tag: &str,
    cycle: usize,
    phase: &str,
    kind: &str,
    counts: &BTreeMap<String, usize>,
) {
    let counts = counts
        .iter()
        .map(|(wallet, count)| format!("{wallet}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        target: TARGET,
        "{tag} cycle {} {phase}: {kind} tx counts by sender wallet: {counts}",
        cycle + 1,
    );
}

#[expect(
    clippy::cognitive_complexity,
    reason = "A dependent burst keeps reservation, signing, diagnostics, shuffle, and submission in order"
)]
#[expect(
    clippy::too_many_lines,
    reason = "A transaction burst needs its complete prepare-shuffle-submit lifecycle together"
)]
#[expect(clippy::too_many_arguments, reason = "Need all args")]
async fn execute_ring_send_round_with_utxo_cache<S: BuildHasher + Sync>(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    transactions_per_wallet: usize,
    value: u64,
    cycle: usize,
    round_number: usize,
    epochs_headroom: u32,
    available_utxos: &mut WalletUtxos,
    used_input_note_ids: &HashSet<NoteId, S>,
    dependent_state: Option<&DependentTransactionLoadState>,
    mempool_diagnostics: bool,
    reservation_retirement: Option<ContinuousWorkloadRetirementContext<'_>>,
    workload_reservations: &mut ContinuousWorkloadReservations,
) -> Result<(HashSet<TxHash>, HashSet<NoteId>), StepError> {
    let policy = build_cycle_fee_policy(world, step, &wallet_names[0], epochs_headroom).await?;
    let mode = if dependent_state.is_some() {
        "CONTINUOUS DEPENDENT NEXT WALLET"
    } else {
        "CONTINUOUS NEXT WALLET"
    };
    let displayed_round = if dependent_state.is_some() {
        round_number
    } else {
        cycle + 1
    };
    let cycle_index_for_logs = displayed_round.saturating_sub(1);
    if reservation_retirement.is_some() {
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "continuous_workload_fee_policy",
            &serde_json::json!({
                "round": displayed_round,
                "epochs_headroom": epochs_headroom,
                "prepared_at_tip": policy.horizon.prepared_at_tip.to_string(),
                "prepared_at_epoch": policy.horizon.prepared_at_epoch.into_inner(),
                "valid_through_epoch": policy.horizon.valid_through_epoch.into_inner(),
                "live_execution_gas_price": policy.horizon.live_prices.execution_base_gas_price.into_inner(),
                "projected_execution_gas_price": policy.horizon.ceiling_prices.execution_base_gas_price.into_inner(),
                "live_storage_gas_price": policy.horizon.live_prices.storage_gas_price.into_inner(),
                "projected_storage_gas_price": policy.horizon.ceiling_prices.storage_gas_price.into_inner(),
                "projected_fee_reserve_percent": policy.priority_fee_percent,
            }),
        );
    }
    if let Some(dependent_state) = dependent_state {
        let transaction_count = wallet_names.len() * transactions_per_wallet;
        dependent_state.begin_burst(round_number, transaction_count);
        info!(
            target: TARGET,
            workload_mode = "dependent",
            burst = round_number,
            transaction_count,
            "Starting dependent next-wallet transaction burst"
        );
    }

    let preparation_started = Instant::now();
    let mut signed_submissions = Vec::with_capacity(wallet_names.len() * transactions_per_wallet);
    let mut prepared_counts = BTreeMap::new();

    for from in wallet_names {
        info!(
            target: TARGET,
            "{mode} round {displayed_round} A: Await funds",
        );

        let required_available = transactions_per_wallet as u64 * value;
        let readiness = WalletSendReadiness::EligibleUtxoBatch {
            min_required_outputs: transactions_per_wallet,
            min_value_per_transaction: value,
        };
        if let Some(context) = reservation_retirement {
            sync::wait_wallet_send_ready_with_callback(
                world,
                step,
                from,
                180,
                required_available,
                readiness,
                available_utxos,
                used_input_note_ids,
                |world| {
                    maintain_continuous_workload_reservations(world, workload_reservations, context)
                        .map(|_| ())
                },
            )
            .await?;
        } else {
            sync::wait_wallet_send_ready(
                world,
                step,
                from,
                180,
                required_available,
                readiness,
                available_utxos,
                used_input_note_ids,
            )
            .await?;
        }
    }

    let pool_build_started = Instant::now();
    let mut workload_pools = utils::WorkloadUtxoPools::from_cache(available_utxos);
    let workload_pool_build_duration = pool_build_started.elapsed();

    let mut reservation_anchors = HashMap::new();
    for i in 0..wallet_names.len() {
        let from = &wallet_names[i];
        let to = &wallet_names[(i + 1) % wallet_names.len()];

        if let Some(context) = reservation_retirement {
            reservation_anchors.insert(
                from.clone(),
                canonical_scanner_anchor(world, context.scanner_group_id)?,
            );
        }

        info!(
            target: TARGET,
            "{mode} round {displayed_round} B: Prepare transactions to next wallet concurrently",
        );
        let mut prepared = prepare_ring_send_round_send_with_utxo_cache(
            world,
            step,
            transactions_per_wallet,
            round_number,
            value,
            from,
            to,
            available_utxos,
            &mut workload_pools,
            Some(policy.horizon.ceiling_prices.clone()),
            policy.priority_fee_percent,
            dependent_state,
            mempool_diagnostics,
        )
        .await?;
        prepared_counts.insert(from.clone(), prepared.len());
        validate_fee_horizon_after_wallet_batch(world, &policy, from, prepared.len()).await?;
        signed_submissions.append(&mut prepared);
    }

    info!(
        target: TARGET,
        workload_mode = if dependent_state.is_some() { "dependent" } else { "independent" },
        round = round_number,
        workload_pool_build_ms = workload_pool_build_duration.as_millis(),
        "Built next-wallet workload UTXO indexes once for the round"
    );

    let mut cycle_used_input_note_ids: HashSet<NoteId> = HashSet::new();
    for submission in &signed_submissions {
        extend_note_id_set(
            &mut cycle_used_input_note_ids,
            &submission.reserved_inputs().input_note_ids_list(),
        );
    }
    let reserved_input_count = signed_submissions
        .iter()
        .map(|submission| submission.reserved_inputs().input_note_ids_list().len())
        .sum::<usize>();
    if cycle_used_input_note_ids.len() != reserved_input_count {
        return Err(StepError::StepFail {
            message: format!(
                "{mode} round {displayed_round} reserved {} unique input note(s) from {reserved_input_count} input selection(s); duplicate inputs were selected",
                cycle_used_input_note_ids.len(),
            ),
        });
    }

    log_phase_counts(
        mode,
        cycle_index_for_logs,
        "C",
        "prepared",
        &prepared_counts,
    );

    let shuffle_seed = shuffle_if_dependent(&mut signed_submissions, dependent_state);
    if let (Some(dependent_state), Some(shuffle_seed)) = (dependent_state, shuffle_seed) {
        dependent_state.set_last_burst_shuffle_seed(shuffle_seed);
        let diagnostics = dependent_state
            .last_burst_diagnostics()
            .expect("dependent burst diagnostics were started before preparation");
        info!(
            target: TARGET,
            workload_mode = "dependent",
            burst = diagnostics.round,
            shuffle_seed,
            transaction_count = diagnostics.transaction_count,
            lineage_counter_start = ?diagnostics.lineage_counter_start,
            lineage_counter_end = ?diagnostics.lineage_counter_end,
            "Prepared and shuffled dependent next-wallet transaction burst"
        );
    }

    let preparation_duration = preparation_started.elapsed();
    info!(
        target: TARGET,
        workload_mode = if dependent_state.is_some() { "dependent" } else { "independent" },
        burst = round_number,
        transaction_count = signed_submissions.len(),
        preparation_and_signing_ms = preparation_duration.as_millis(),
        "Finished transaction preparation before burst submission"
    );
    let workload_mode = if dependent_state.is_some() {
        "dependent"
    } else {
        "independent"
    };
    info!(
        target: TARGET,
        workload_mode,
        burst = round_number,
        transaction_count = signed_submissions.len(),
        "Starting burst submission phase"
    );
    let network_result = utils::submit_signed_user_wallet_submissions_to_nodes(
        &*world,
        signed_submissions,
        Some(&policy),
    )
    .await?;
    let SignedUserWalletSubmissionNetworkResult {
        accepted,
        first_error,
        failed_transaction_count,
        fanout_node_names,
        node_selection_duration,
        preflight_duration,
        network_submission_duration,
    } = network_result;
    info!(
        target: TARGET,
        workload_mode,
        burst = round_number,
        accepted = accepted.len(),
        failed = failed_transaction_count,
        fanout_nodes = ?fanout_node_names,
        network_submission_ms = network_submission_duration.as_millis(),
        "Burst network acceptance completed"
    );
    let mut submitted_counts = BTreeMap::new();
    let pending_snapshot_result = if mempool_diagnostics {
        info!(
            target: TARGET,
            workload_mode,
            burst = round_number,
            "Capturing post-network acceptance mempool snapshot before wallet bookkeeping"
        );
        Some(
            record_mempool_pending_counts(
                world,
                workload_mode,
                &format!("burst_{round_number}_submitted"),
            )
            .await,
        )
    } else {
        None
    };

    info!(
        target: TARGET,
        workload_mode,
        burst = round_number,
        transaction_count = accepted.len(),
        "Starting burst wallet bookkeeping"
    );
    let bookkeeping_started = Instant::now();
    let submitted_hashes = utils::record_accepted_signed_user_wallet_submissions(world, &accepted)?;
    if reservation_retirement.is_some() {
        let submitted_wallets = submitted_hashes
            .iter()
            .map(|(wallet_name, tx_hash)| (*tx_hash, wallet_name.clone()))
            .collect::<HashMap<_, _>>();
        for submission in &accepted {
            let tx_hash = submission.tx_hash();
            let wallet_name = submitted_wallets.get(&tx_hash).ok_or_else(|| {
                StepError::LogicalError {
                    message: format!(
                        "accepted continuous workload transaction {tx_hash:?} has no wallet bookkeeping record"
                    ),
                }
            })?;
            let (reserved_height, reserved_tip) = reservation_anchors
                .get(wallet_name)
                .copied()
                .ok_or_else(|| StepError::LogicalError {
                    message: format!(
                        "continuous workload wallet '{wallet_name}' has no scanner reservation anchor"
                    ),
                })?;
            let (inputs, _) = submission
                .reserved_inputs()
                .into_sender_and_fee_sponsor_inputs();
            workload_reservations.register(
                tx_hash,
                wallet_name.clone(),
                inputs,
                reserved_height,
                reserved_tip,
            );
        }
    }
    let bookkeeping_duration = bookkeeping_started.elapsed();
    info!(
        target: TARGET,
        workload_mode,
        burst = round_number,
        transaction_count = submitted_hashes.len(),
        wallet_bookkeeping_ms = bookkeeping_duration.as_millis(),
        "Completed burst wallet bookkeeping"
    );
    for (sender, _) in &submitted_hashes {
        *submitted_counts.entry(sender.clone()).or_insert(0usize) += 1;
    }
    log_phase_counts(
        mode,
        cycle_index_for_logs,
        "D",
        "submitted",
        &submitted_counts,
    );

    if let Some(Err(snapshot_error)) = pending_snapshot_result {
        if let Some(network_error) = first_error {
            return Err(StepError::StepFail {
                message: format!(
                    "Post-network diagnostic snapshot failed ({snapshot_error}) after \
                    {failed_transaction_count} transaction(s) failed all fan-out nodes; first \
                    submission error: {network_error}"
                ),
            });
        }
        return Err(snapshot_error);
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    if mempool_diagnostics {
        let dependent_diagnostics =
            dependent_state.and_then(DependentTransactionLoadState::last_burst_diagnostics);
        BlendDiagnosticEventLogger::from_world(world).append_named_timeline_record(
            "mempool_transaction_burst_submitted",
            &serde_json::json!({
                "workload_mode": workload_mode,
                "round": round_number,
                "transaction_count": submitted_hashes.len(),
                "reserved_input_count": reserved_input_count,
                "preparation_and_signing_ms": preparation_duration.as_millis(),
                "shuffle_seed": dependent_diagnostics.as_ref().and_then(|diagnostics| diagnostics.shuffle_seed),
                "node_selection_ms": node_selection_duration.as_millis(),
                "fanout_nodes": fanout_node_names,
                "preflight_ms": preflight_duration.as_millis(),
                "network_submission_ms": network_submission_duration.as_millis(),
                "wallet_bookkeeping_ms": bookkeeping_duration.as_millis(),
                "failed_transaction_count": failed_transaction_count,
                "total_submit_and_record_ms": node_selection_duration
                    .saturating_add(preflight_duration)
                    .saturating_add(network_submission_duration)
                    .saturating_add(bookkeeping_duration)
                    .as_millis(),
                "lineage_counter_start": dependent_diagnostics.as_ref().and_then(|diagnostics| diagnostics.lineage_counter_start),
                "lineage_counter_end": dependent_diagnostics.as_ref().and_then(|diagnostics| diagnostics.lineage_counter_end),
            }),
        );
    }
    let cycle_tx_hashes = submitted_hashes
        .into_iter()
        .map(|(_, tx_hash)| tx_hash)
        .collect::<HashSet<_>>();

    Ok((cycle_tx_hashes, cycle_used_input_note_ids))
}

#[expect(clippy::too_many_lines, reason = "Test function.")]
async fn execute_non_stop_manual_command(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    match command {
        ManualCommand::CreateSnapshotAllNodes { snapshot_name } => {
            create_snapshot_all_nodes_with_wallet_state(world, snapshot_name).await
        }
        ManualCommand::CreateSnapshotNode {
            snapshot_name,
            node_name,
        } => create_snapshot_node_with_wallet_state(world, snapshot_name, node_name).await,
        ManualCommand::CoinSplit {
            wallet,
            outputs,
            value,
        } => execute_coin_split(world, step, wallet, *outputs, *value)
            .await
            .map(|_| ()),
        ManualCommand::Verify { .. } => handle_verify_command(world, step, command).await,
        ManualCommand::WalletBalance { wallet_name } => {
            log_wallet_balance(world, step, wallet_name).await
        }
        ManualCommand::WalletBalanceAllUserWallets => {
            log_wallet_balances(world, step, world.all_user_wallets()).await
        }
        ManualCommand::WalletBalanceAllFundingWallets => {
            log_wallet_balances(world, step, world.all_node_wallets()).await
        }
        ManualCommand::WalletBalanceAllWallets => {
            let mut wallets = world.all_user_wallets();
            wallets.extend(world.all_node_wallets());

            log_wallet_balances(world, step, wallets).await
        }
        ManualCommand::ExportFunds {
            wallet_name,
            value,
            output_path,
            include_secret,
        } => {
            export_funds(
                world,
                step,
                wallet_name,
                *value,
                output_path,
                *include_secret,
            )
            .await
        }
        ManualCommand::ClearEncumbrances { wallet_name } => {
            clear_wallet_encumbrances(world, step, wallet_name)
        }
        ManualCommand::ClearEncumbrancesAllWallets => clear_all_wallet_encumbrances(world, step),
        ManualCommand::Send {
            num_transactions,
            value,
            from,
            to,
        } => execute_send(world, step, *num_transactions, *value, from, to).await,
        ManualCommand::Drain { from, to } => execute_drain(world, step, from, to).await,
        ManualCommand::DrainAllNodeWallets { node_name, to } => {
            drain_all_node_wallets(world, node_name, to).await
        }
        ManualCommand::ContinuousRoundRobinUserWallets { .. } => {
            execute_continuous_round_robin(world, step, command).await
        }
        ManualCommand::FaucetFundsAllUserWallets { rounds } => {
            request_faucet_funds_all_user_wallets(world, step, *rounds)
        }
        ManualCommand::FaucetFundsAllFundingWallets { rounds } => {
            request_faucet_funds_all_funding_wallets(world, step, *rounds)
        }
        ManualCommand::RestartNode { node_name } => restart_node(world, step, node_name).await,
        ManualCommand::CryptarchiaInfoAllNodes => {
            nodes::get_cryptarchia_info_all_nodes(world, step).await;
            Ok(())
        }
        ManualCommand::WaitAllNodesSyncedToChain => {
            wait_for_all_nodes_to_be_synced_to_chain(world, step).await
        }
        ManualCommand::CoinSplitAllUserWallets {
            splits_per_wallet,
            outputs,
            value,
        } => {
            execute_coin_splits_all_user_wallets(world, step, *splits_per_wallet, *outputs, *value)
                .await
        }
        ManualCommand::VerifyMinAvailableOutputsAllUserWallets {
            min_outputs,
            timeout_seconds,
        } => {
            verify_min_outputs_all_user_wallets(
                world,
                step,
                *min_outputs,
                *timeout_seconds,
                WalletOutputState::Available,
            )
            .await
        }
        ManualCommand::ContinuousNextWalletUserWallets { .. } => {
            execute_continuous_next_wallet_user_wallet(world, step, command).await
        }
        ManualCommand::Stop => Ok(()),
    }
}

#[cfg(test)]
mod continuous_round_completion_tests {
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use lb_core::{header::HeaderId, mantle::TxHash};
    use tokio::{sync::watch, task::yield_now, time::timeout};

    use super::{
        CONTINUOUS_ROUND_INCLUSION_TIMEOUT, OPEN_BLOCK_MAX_TRANSACTIONS,
        evaluate_continuous_round_completion, wait_for_continuous_round_completion,
        wait_for_continuous_round_completion_with_timeout,
    };
    use crate::{
        common::wallet::scanner::state::{ScannerBlockObservation, WalletScannerState},
        cucumber::error::StepError,
    };

    fn expected_hashes(count: usize) -> HashSet<TxHash> {
        (0..count)
            .map(|index| TxHash([u8::try_from(index).expect("test hash index fits u8"); 32]))
            .collect()
    }

    fn block(
        height: u64,
        transaction_count: usize,
        hashes: Vec<TxHash>,
    ) -> ScannerBlockObservation {
        ScannerBlockObservation {
            height,
            block_id: HeaderId::from([u8::try_from(height).unwrap_or_default(); 32]),
            transaction_count,
            transaction_hashes: hashes,
        }
    }

    #[test]
    fn all_submitted_transactions_included_completes_the_round() {
        let expected = expected_hashes(200);
        let result = evaluate_continuous_round_completion(
            &expected,
            &[block(1, 200, expected.iter().copied().collect())],
        );

        let result = result.expect("full inclusion completes immediately");
        assert_eq!(result.included_transaction_count, 200);
        assert_eq!(result.outstanding_transaction_count, 0);
        assert_eq!(result.included_transaction_hashes, expected);
        assert!(result.outstanding_transaction_hashes.is_empty());
        assert_eq!(result.reason, "all_transactions_included");
    }

    #[test]
    fn less_than_half_inclusion_never_enables_open_block_completion() {
        let expected = expected_hashes(200);
        let included: Vec<_> = expected.iter().take(99).copied().collect();
        let blocks = (1..=6)
            .map(|height| {
                block(
                    height,
                    OPEN_BLOCK_MAX_TRANSACTIONS,
                    if height == 1 {
                        included.clone()
                    } else {
                        Vec::new()
                    },
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &blocks,),
            None
        );
    }

    #[test]
    fn exactly_half_inclusion_needs_three_later_open_blocks() {
        let expected = expected_hashes(200);
        let included = expected.iter().take(100).copied().collect::<Vec<_>>();
        let one_open = [
            block(1, 100, included),
            block(2, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &one_open),
            None
        );

        let two_open = [
            one_open[0].clone(),
            one_open[1].clone(),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &two_open),
            None
        );

        let three_open = [
            two_open[0].clone(),
            two_open[1].clone(),
            two_open[2].clone(),
            block(4, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        let result = evaluate_continuous_round_completion(&expected, &three_open)
            .expect("third qualifying subsequent open block completes the round");
        assert_eq!(result.included_transaction_count, 100);
        assert_eq!(result.outstanding_transaction_count, 100);
        assert_eq!(result.reason, "three_subsequent_open_blocks");
        assert_eq!(result.included_transaction_hashes.len(), 100);
        assert_eq!(result.outstanding_transaction_hashes.len(), 100);
    }

    #[test]
    fn blocks_over_open_threshold_are_ignored_without_resetting_open_block_count() {
        let expected = expected_hashes(200);
        let included = expected.iter().take(100).copied().collect::<Vec<_>>();
        let observations = [
            block(1, 100, included),
            block(2, 1_024, Vec::new()),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, 973, Vec::new()),
            block(5, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &observations),
            None
        );
        let mut with_third_open = observations.to_vec();
        with_third_open.push(block(6, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()));
        let result = evaluate_continuous_round_completion(&expected, &with_third_open);
        assert_eq!(
            result.map(|result| result.reason),
            Some("three_subsequent_open_blocks")
        );
    }

    #[test]
    fn odd_sized_round_uses_ceiling_for_half_threshold() {
        let expected = expected_hashes(5);
        let below = expected.iter().take(2).copied().collect::<Vec<_>>();
        let below_threshold = [
            block(1, 2, below.clone()),
            block(2, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &below_threshold),
            None
        );

        let third = expected.iter().nth(2).copied().expect("third hash exists");
        let reached = [
            block(1, 2, below),
            block(2, 1, vec![third]),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(5, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &reached)
                .map(|result| result.included_transaction_count),
            Some(3)
        );
    }

    #[test]
    fn further_inclusion_resets_open_block_count() {
        let expected = expected_hashes(200);
        let first_inclusions = expected.iter().take(100).copied().collect::<Vec<_>>();
        let next_inclusion = expected.iter().nth(100).copied().expect("next hash exists");
        let observations = [
            block(1, 100, first_inclusions),
            block(2, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, 1, vec![next_inclusion]),
            block(5, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(6, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &observations),
            None,
            "new inclusion resets the open-block count"
        );
        let mut third_open_after_new_inclusion = observations.to_vec();
        third_open_after_new_inclusion.push(block(7, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()));
        let result =
            evaluate_continuous_round_completion(&expected, &third_open_after_new_inclusion)
                .expect("third open block after the newest inclusion completes");
        assert_eq!(result.included_transaction_count, 101);
        assert_eq!(result.outstanding_transaction_count, 99);
    }

    #[test]
    fn reorg_rollback_removes_inclusion_and_open_block_evidence() {
        let expected = expected_hashes(200);
        let half = expected.iter().take(100).copied().collect::<Vec<_>>();
        let old_chain = [
            block(1, 100, half.clone()),
            block(2, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert!(evaluate_continuous_round_completion(&expected, &old_chain).is_some());

        let replacement_chain = [
            block(1, 99, half.into_iter().take(99).collect()),
            block(2, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(3, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
            block(4, OPEN_BLOCK_MAX_TRANSACTIONS, Vec::new()),
        ];
        assert_eq!(
            evaluate_continuous_round_completion(&expected, &replacement_chain),
            None
        );
    }

    #[tokio::test]
    async fn continuous_round_wait_stops_promptly_on_cancellation() {
        let scanner_state = Arc::new(Mutex::new(WalletScannerState::default()));
        let expected = expected_hashes(1);
        let (cancellation_sender, mut cancellation_receiver) = watch::channel(false);

        let result = timeout(Duration::from_secs(1), async {
            let wait = wait_for_continuous_round_completion(
                scanner_state,
                "",
                "cancellation regression",
                &expected,
                1,
                Some(1),
                Some(&mut cancellation_receiver),
            );
            tokio::pin!(wait);

            tokio::select! {
                biased;
                result = &mut wait => panic!("round wait returned before cancellation: {result:?}"),
                () = yield_now() => {}
            }

            cancellation_sender
                .send(true)
                .expect("the round wait should still hold the receiver");
            wait.await
        })
        .await
        .expect("cancelled round wait should return promptly");

        assert!(matches!(result, Err(StepError::BackgroundTaskCancelled)));
    }

    #[tokio::test]
    async fn continuous_round_wait_keeps_the_stagnation_timeout() {
        let scanner_state = Arc::new(Mutex::new(WalletScannerState::default()));
        let mut group = crate::common::wallet::scanner::state::ForkGroupScannerState::new(
            String::new(),
            "wallet".to_owned(),
            1,
        );
        group.status = crate::common::wallet::scanner::state::ScannerStatus::Tailing;
        scanner_state
            .lock()
            .expect("scanner state lock")
            .groups
            .insert(String::new(), group);
        let expected = expected_hashes(2);
        let result = wait_for_continuous_round_completion_with_timeout(
            scanner_state,
            "",
            "stagnation regression",
            &expected,
            2,
            Some(1),
            None,
            Duration::from_millis(1),
            None,
        )
        .await;
        assert!(matches!(result, Err(StepError::Timeout { .. })));
        assert_eq!(CONTINUOUS_ROUND_INCLUSION_TIMEOUT, Duration::from_mins(10));
    }
}

#[cfg(test)]
mod continuous_workload_reservation_tests {
    use std::collections::{HashMap, HashSet};

    use lb_core::{
        header::HeaderId,
        mantle::{Note, TxHash, Utxo},
    };
    use lb_key_management_system_service::keys::ZkPublicKey;
    use lb_testing_framework::configs::wallet::WalletAccount;

    use super::{
        ContinuousRoundCompletion, ContinuousWorkloadReservations,
        ContinuousWorkloadRetirementContext,
        execute_continuous_next_wallet_user_wallet_with_cancellation,
        maintain_continuous_workload_reservations, verify_next_wallet_round_completion,
    };
    use crate::{
        common::wallet::{
            TrackedWalletKeys, WalletOutputState, WalletReservedInputs,
            scanner::state::{ForkGroupScannerState, ScannerBlockObservation},
        },
        cucumber::{
            error::StepError,
            steps::transactions::manual_control::parsing::ManualCommand,
            world::{CucumberWorld, WalletInfo, WalletType},
        },
    };
    fn input(value: u64, index: usize) -> Utxo {
        Utxo::new(
            [u8::try_from(index).expect("test input index fits u8"); 32],
            index,
            Note::new(value, ZkPublicKey::zero()),
        )
    }

    fn block_id(seed: u8) -> HeaderId {
        HeaderId::from([seed; 32])
    }

    fn outstanding_completion(
        hashes: impl IntoIterator<Item = TxHash>,
    ) -> ContinuousRoundCompletion {
        let outstanding_transaction_hashes = hashes.into_iter().collect::<HashSet<_>>();
        ContinuousRoundCompletion {
            included_transaction_hashes: HashSet::new(),
            included_transaction_count: 0,
            outstanding_transaction_count: outstanding_transaction_hashes.len(),
            outstanding_transaction_hashes,
            reason: "three_subsequent_open_blocks",
        }
    }

    fn canonical_blocks(entries: &[(u64, HeaderId)]) -> HashMap<u64, HeaderId> {
        entries.iter().copied().collect()
    }

    #[test]
    fn outstanding_reservation_waits_until_k_canonical_blocks_then_becomes_releasable() {
        let tx_hash = TxHash([1; 32]);
        let input = input(25, 1);
        let anchor = block_id(10);
        let mut reservations = ContinuousWorkloadReservations::default();
        reservations.register(tx_hash, "wallet".to_owned(), vec![input], 100, Some(anchor));

        let (active, candidates) = reservations.observe_canonical_chain(
            105,
            Some(block_id(15)),
            &canonical_blocks(&[(100, anchor), (105, block_id(15))]),
            &HashSet::new(),
            10,
        );
        assert_eq!(active.eligible_reservation_count, 0);
        assert!(candidates.is_empty());

        reservations.finish_round(&outstanding_completion([tx_hash]));

        let (before_k, candidates) = reservations.observe_canonical_chain(
            109,
            Some(block_id(19)),
            &canonical_blocks(&[(100, anchor), (109, block_id(19))]),
            &HashSet::new(),
            10,
        );
        assert!(candidates.is_empty());
        assert_eq!(before_k.eligible_reservation_count, 1);
        assert_eq!(before_k.age_eligible_reservation_count, 0);

        let (_, candidates) = reservations.observe_canonical_chain(
            110,
            Some(block_id(20)),
            &canonical_blocks(&[(100, anchor), (110, block_id(20))]),
            &HashSet::new(),
            10,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tx_hash, tx_hash);
        assert_eq!(candidates[0].inputs, vec![input]);
    }

    #[test]
    fn reorged_reservation_anchor_restarts_k_block_age() {
        let tx_hash = TxHash([2; 32]);
        let input = input(30, 2);
        let old_anchor = block_id(10);
        let new_anchor = block_id(99);
        let mut reservations = ContinuousWorkloadReservations::default();
        reservations.register(
            tx_hash,
            "wallet".to_owned(),
            vec![input],
            100,
            Some(old_anchor),
        );
        reservations.finish_round(&outstanding_completion([tx_hash]));

        let (reorg, candidates) = reservations.observe_canonical_chain(
            110,
            Some(new_anchor),
            &canonical_blocks(&[(100, new_anchor), (110, block_id(20))]),
            &HashSet::new(),
            10,
        );
        assert_eq!(reorg.reorg_reanchored_reservation_count, 1);
        assert!(candidates.is_empty());

        let (_, candidates) = reservations.observe_canonical_chain(
            119,
            Some(block_id(29)),
            &canonical_blocks(&[(110, new_anchor), (119, block_id(29))]),
            &HashSet::new(),
            10,
        );
        assert!(candidates.is_empty());

        let (_, candidates) = reservations.observe_canonical_chain(
            120,
            Some(block_id(30)),
            &canonical_blocks(&[(110, new_anchor), (120, block_id(30))]),
            &HashSet::new(),
            10,
        );
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn included_transactions_clear_tracking_and_other_active_owners_protect_inputs() {
        let mined_hash = TxHash([3; 32]);
        let shared_hash = TxHash([4; 32]);
        let shared_input = input(40, 3);
        let anchor = block_id(10);
        let mut reservations = ContinuousWorkloadReservations::default();
        reservations.register(
            mined_hash,
            "wallet".to_owned(),
            vec![input(20, 4)],
            100,
            Some(anchor),
        );
        reservations.register(
            shared_hash,
            "wallet".to_owned(),
            vec![shared_input],
            100,
            Some(anchor),
        );
        reservations.finish_round(&outstanding_completion([mined_hash, shared_hash]));

        let (summary, candidates) = reservations.observe_canonical_chain(
            110,
            Some(block_id(20)),
            &canonical_blocks(&[(100, anchor), (110, block_id(20))]),
            &HashSet::from([mined_hash]),
            10,
        );
        assert_eq!(summary.included_reservation_count, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tx_hash, shared_hash);
        assert_eq!(candidates[0].inputs, vec![shared_input]);
        assert!(!reservations.active.contains_key(&mined_hash));
    }

    #[test]
    fn another_active_transaction_owner_prevents_releasing_a_shared_input() {
        let first_hash = TxHash([5; 32]);
        let second_hash = TxHash([6; 32]);
        let shared_input = input(50, 5);
        let anchor = block_id(10);
        let mut reservations = ContinuousWorkloadReservations::default();
        reservations.register(
            first_hash,
            "wallet".to_owned(),
            vec![shared_input],
            100,
            Some(anchor),
        );
        reservations.register(
            second_hash,
            "wallet".to_owned(),
            vec![shared_input],
            100,
            Some(anchor),
        );
        reservations.finish_round(&outstanding_completion([first_hash, second_hash]));

        let (_, candidates) = reservations.observe_canonical_chain(
            110,
            Some(block_id(20)),
            &canonical_blocks(&[(100, anchor), (110, block_id(20))]),
            &HashSet::new(),
            10,
        );
        assert!(candidates.is_empty());
        assert_eq!(reservations.active.len(), 2);

        let (_, candidates) = reservations.observe_canonical_chain(
            110,
            Some(block_id(20)),
            &canonical_blocks(&[(100, anchor), (110, block_id(20))]),
            &HashSet::from([first_hash]),
            10,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tx_hash, second_hash);
        assert_eq!(candidates[0].inputs, vec![shared_input]);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Two round completions exercise deferred retirement, wallet balances, and later inclusion with shared tracking."
    )]
    #[tokio::test]
    async fn deferred_retirement_and_late_inclusions_survive_executor_calls() {
        let directory = tempfile::tempdir().expect("diagnostic directory exists");
        let mut world = CucumberWorld::default();
        world.lifecycle.scenario_base_dir = directory.path().to_path_buf();
        world.scanner.runtime_is_shared = true;
        world
            .startup
            .manual_node_config_overrides
            .cryptarchia_security_param = std::num::NonZeroU32::new(10);
        for (index, name) in [(1, "wallet"), (2, "recipient")] {
            world.wallet_registry.wallet_info.insert(
                name.to_owned(),
                WalletInfo {
                    wallet_name: name.to_owned(),
                    node_name: "NODE_9".to_owned(),
                    wallet_type: WalletType::User {
                        wallet_account: WalletAccount::deterministic(index, 100, false)
                            .expect("test account exists"),
                    },
                },
            );
        }
        // Stop preparation at fee-policy validation after the executor's initial
        // maintenance pass, without requiring live nodes or submitting a batch.
        let command = ManualCommand::ContinuousNextWalletUserWallets {
            cycles: 1,
            num_transactions: 1,
            value: 1,
            epochs_headroom: 17,
        };
        let (_cancellation_sender, mut cancellation) = tokio::sync::watch::channel(false);
        let first_included = TxHash([1; 32]);
        let outstanding = TxHash([2; 32]);
        let next_included = TxHash([3; 32]);
        let unrelated = TxHash([4; 32]);
        let stale_input = input(25, 1);
        let unrelated_input = input(40, 2);
        world
            .with_wallets_mut(|wallets| {
                wallets.replace_current_wallets_utxos([(
                    "wallet".into(),
                    vec![stale_input, unrelated_input],
                )]);
                for (hash, input) in [(outstanding, stale_input), (unrelated, unrelated_input)] {
                    wallets.record_wallet_reservation(
                        "wallet",
                        hash,
                        WalletReservedInputs::new(vec![input], Vec::new()),
                        0,
                    );
                }
            })
            .expect("wallets can be updated");

        let block = |height: u8, hashes: Vec<TxHash>| ScannerBlockObservation {
            height: u64::from(height),
            block_id: block_id(height),
            transaction_count: hashes.len(),
            transaction_hashes: hashes,
        };
        let mut group = ForkGroupScannerState::new(String::new(), "wallet".to_owned(), 1);
        group.recent_blocks = (100..=106)
            .map(|height| {
                block(
                    height,
                    if height == 103 {
                        vec![first_included]
                    } else {
                        Vec::new()
                    },
                )
            })
            .collect();
        let context = ContinuousWorkloadRetirementContext {
            scanner_group_id: "",
            security_parameter: 10,
        };
        let mut reservations = ContinuousWorkloadReservations::default();
        for (hash, input) in [(outstanding, stale_input), (unrelated, unrelated_input)] {
            reservations.register(
                hash,
                "wallet".to_owned(),
                vec![input],
                100,
                Some(block_id(100)),
            );
        }

        for (round, height, hashes) in [
            (1, 106, HashSet::from([first_included, outstanding])),
            (2, 110, HashSet::from([next_included])),
        ] {
            if round == 2 {
                group.recent_blocks.extend((107..=110).map(|height| {
                    block(
                        height,
                        if height == 107 {
                            vec![next_included]
                        } else {
                            Vec::new()
                        },
                    )
                }));
            }
            group.applied_height = u64::from(height);
            group.target_height = Some(group.applied_height);
            group.applied_tip = Some(block_id(height));
            world
                .scanner
                .state
                .lock()
                .expect("scanner lock")
                .groups
                .insert(String::new(), group.clone());
            let execution = execute_continuous_next_wallet_user_wallet_with_cancellation(
                &mut world,
                "cross-round retirement",
                &command,
                &mut cancellation,
                &mut reservations,
            )
            .await;
            assert!(
                matches!(execution, Err(StepError::InvalidArgument { message })
                if message == "epochs headroom per cycle must be at most 16")
            );
            assert_eq!(reservations.retired.contains(&outstanding), round == 2);
            let completion = verify_next_wallet_round_completion(
                &mut world,
                "cross-round retirement",
                &hashes,
                hashes.len(),
                Some(round),
                "continuous workload",
                None,
                Some(""),
                Some(context),
                &mut reservations,
            )
            .await
            .expect("canonical blocks complete the round");
            assert_eq!(completion.included_transaction_count, 1);
            assert_eq!(
                completion.outstanding_transaction_count,
                usize::from(round == 1)
            );
            assert_eq!(
                completion.reason,
                if round == 1 {
                    "three_subsequent_open_blocks"
                } else {
                    "all_transactions_included"
                }
            );
            reservations.finish_round(&completion);
            maintain_continuous_workload_reservations(&world, &mut reservations, context)
                .expect("reservations are maintained");

            let wallet = world
                .with_wallets(|wallets| {
                    wallets
                        .current_wallet_states([TrackedWalletKeys::new(
                            "wallet",
                            [ZkPublicKey::zero()],
                        )])
                        .into_values()
                        .next()
                        .expect("wallet exists")
                })
                .expect("wallet state is readable");
            let on_chain = wallet.balance(WalletOutputState::OnChain);
            let reserved = wallet.balance(WalletOutputState::Reserved);
            let available = wallet.balance(WalletOutputState::Available);
            assert_eq!((on_chain.output_count, on_chain.value), (2, 65));
            assert_eq!(
                (
                    reserved.output_count,
                    reserved.value,
                    available.output_count,
                    available.value
                ),
                if round == 1 {
                    (2, 65, 0, 0)
                } else {
                    (1, 40, 1, 25)
                }
            );
            assert_eq!(reservations.active.contains_key(&outstanding), round == 1);
            assert!(reservations.active.contains_key(&unrelated));
        }
        assert_eq!(reservations.retired_input_count, 1);
        assert_eq!(reservations.retired_input_value, 25);
        group.applied_height = 111;
        group.applied_tip = Some(block_id(111));
        group.recent_blocks.push(block(111, vec![outstanding]));
        world
            .with_wallets_mut(|wallets| {
                wallets.replace_current_wallets_utxos([("wallet".into(), vec![unrelated_input])]);
            })
            .expect("scanner reconciles the spent input");
        world
            .scanner
            .state
            .lock()
            .expect("scanner lock")
            .groups
            .insert(String::new(), group);
        let late = maintain_continuous_workload_reservations(&world, &mut reservations, context)
            .expect("late inclusion is observed");
        assert_eq!(late.late_inclusion_tx_hashes, vec![outstanding]);
        let repeated =
            maintain_continuous_workload_reservations(&world, &mut reservations, context)
                .expect("repeated observation is safe");
        assert_eq!(repeated.late_inclusion_tx_hashes, Vec::<TxHash>::new());
    }
}
