use std::time::Instant;

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
use crate::cucumber::{
    steps::{
        mempool::steps::record_mempool_pending_counts,
        nodes::diagnostics::BlendDiagnosticEventLogger,
    },
    wallet::submissions::SignedUserWalletSubmissionNetworkResult,
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
    execute_continuous_next_wallet_user_wallet_inner(world, step, command, None, None, false, 1)
        .await
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
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        &command,
        None,
        dependent_state.as_ref(),
        true,
        1,
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
) -> Result<(), StepError> {
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        command,
        Some(cancellation),
        None,
        false,
        1,
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
) -> Result<(), StepError> {
    execute_continuous_next_wallet_user_wallet_inner(
        world,
        step,
        command,
        Some(cancellation),
        Some(dependent_state),
        false,
        first_round,
    )
    .await
}

struct NextWalletRoundCompletion {
    round_number: usize,
    transaction_count: usize,
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
        inclusion_verification_ms = completion.inclusion_verification_duration.as_millis(),
        total_round_ms = completion.total_round_duration.as_millis(),
        shuffle_seed = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.shuffle_seed),
        lineage_counter_start = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_start),
        lineage_counter_end = ?dependent_diagnostics.and_then(|diagnostics| diagnostics.lineage_counter_end),
        "Verified mempool transaction load round"
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

async fn execute_continuous_next_wallet_user_wallet_inner(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
    mut cancellation: Option<&mut tokio::sync::watch::Receiver<bool>>,
    dependent_state: Option<&DependentTransactionLoadState>,
    mempool_diagnostics: bool,
    first_round: usize,
) -> Result<(), StepError> {
    let (cycles, transactions_per_wallet, value, epochs_headroom) =
        destructure_next_wallet_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let (mut used_input_note_ids, mut all_next_wallet_tx_hashes) = (HashSet::new(), HashSet::new());
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
        log_next_wallet_round_started(
            world,
            mempool_diagnostics,
            workload_mode,
            round_number,
            wallet_names.len() * transactions_per_wallet,
        );
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
        verify_transactions_mined(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * transactions_per_wallet,
            Some(displayed_round),
            mode,
            "D",
            cancellation.as_deref_mut(),
        )
        .await?;
        let verification_duration = verification_started.elapsed();
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
        "{mode} scenario complete: {} unique submitted transaction(s) verified \
        across {} cycle(s)",
        all_next_wallet_tx_hashes.len(),
        cycles,
    );

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
        sync::wait_wallet_send_ready(
            world,
            step,
            from,
            180,
            required_available,
            WalletSendReadiness::EligibleUtxoBatch {
                min_required_outputs: transactions_per_wallet,
                min_value_per_transaction: value,
            },
            available_utxos,
            used_input_note_ids,
        )
        .await?;
    }

    let pool_build_started = Instant::now();
    let mut workload_pools = utils::WorkloadUtxoPools::from_cache(available_utxos);
    let workload_pool_build_duration = pool_build_started.elapsed();

    for i in 0..wallet_names.len() {
        let from = &wallet_names[i];
        let to = &wallet_names[(i + 1) % wallet_names.len()];

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
