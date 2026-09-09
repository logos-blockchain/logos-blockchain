use super::{
    BTreeMap, BuildHasher, CucumberWorld, Duration, HashSet, ManualCommand, NoteId, StepError,
    TARGET, TxHash, WalletOutputState, WalletSendReadiness, WalletUtxos, all_user_wallets,
    build_cycle_fee_policy, clear_all_wallet_encumbrances, clear_wallet_encumbrances,
    create_snapshot_all_nodes_with_wallet_state, create_snapshot_node_with_wallet_state,
    current_available_utxos_for_user_wallets, drain_all_node_wallets, execute_coin_split,
    execute_coin_split_with_utxo_cache, execute_continuous_round_robin, execute_drain,
    execute_send, export_funds, extend_note_id_set, extend_tx_hash_set, handle_verify_command,
    info, log_wallet_balance, log_wallet_balances, nodes,
    prepare_ring_send_round_send_with_utxo_cache, request_faucet_funds_all_funding_wallets,
    request_faucet_funds_all_user_wallets, restart_node, sync, utils,
    validate_fee_horizon_after_wallet_batch, verify_no_duplicate_transactions,
    wait_for_all_nodes_to_be_synced_to_chain, wait_for_observed_transaction_hashes,
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
    execute_continuous_next_wallet_user_wallet_inner(world, step, command).await
}

async fn execute_continuous_next_wallet_user_wallet_inner(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let (cycles, transactions_per_wallet, value, epochs_headroom) =
        destructure_next_wallet_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let mut used_input_note_ids: HashSet<NoteId> = HashSet::new();
    let mut all_next_wallet_tx_hashes = HashSet::new();
    for cycle in 0..cycles {
        let mut available_utxos = current_available_utxos_for_user_wallets(world, step).await?;

        let (cycle_tx_hashes, cycle_used_input_note_ids) = execute_ring_send_round_with_utxo_cache(
            world,
            step,
            &wallet_names,
            transactions_per_wallet,
            value,
            cycle,
            epochs_headroom,
            &mut available_utxos,
            &used_input_note_ids,
        )
        .await?;
        verify_no_duplicate_transactions(
            &cycle_tx_hashes,
            &all_next_wallet_tx_hashes,
            cycle,
            "CONTINUOUS NEXT WALLET",
        )?;
        extend_note_id_set(&mut used_input_note_ids, &cycle_used_input_note_ids);

        verify_transactions_mined(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * transactions_per_wallet,
            Some(cycle + 1),
            "CONTINUOUS NEXT WALLET",
            "D",
        )
        .await?;
        extend_tx_hash_set(&mut all_next_wallet_tx_hashes, &cycle_tx_hashes);
    }

    let expected_total = cycles * wallet_names.len() * transactions_per_wallet;
    if all_next_wallet_tx_hashes.len() != expected_total {
        return Err(StepError::StepFail {
            message: format!(
                "CONTINUOUS NEXT WALLET submitted {} unique transaction hash(es), expected \
                {expected_total}",
                all_next_wallet_tx_hashes.len(),
            ),
        });
    }

    info!(
        target: TARGET,
        "CONTINUOUS NEXT WALLET scenario complete: {} unique submitted transaction(s) verified \
        across {} cycle(s)",
        all_next_wallet_tx_hashes.len(),
        cycles,
    );

    Ok(())
}

pub(super) async fn verify_transactions_mined(
    world: &mut CucumberWorld,
    step: &str,
    tx_hashes: &HashSet<TxHash>,
    expected_tx_count: usize,
    cycle: Option<usize>,
    tag: &str,
    phase: &str,
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

    wait_for_observed_transaction_hashes(world, step, tx_hashes, Duration::from_mins(10)).await
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

#[expect(clippy::too_many_arguments, reason = "Need all args")]
async fn execute_ring_send_round_with_utxo_cache<S: BuildHasher + Sync>(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    transactions_per_wallet: usize,
    value: u64,
    cycle: usize,
    epochs_headroom: u32,
    available_utxos: &mut WalletUtxos,
    used_input_note_ids: &HashSet<NoteId, S>,
) -> Result<(HashSet<TxHash>, HashSet<NoteId>), StepError> {
    let policy = build_cycle_fee_policy(world, step, &wallet_names[0], epochs_headroom).await?;

    let mut signed_submissions = Vec::with_capacity(wallet_names.len() * transactions_per_wallet);
    let mut prepared_counts = BTreeMap::new();

    for i in 0..wallet_names.len() {
        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} A: Await funds",
            cycle + 1
        );
        let from = &wallet_names[i];
        let to = &wallet_names[(i + 1) % wallet_names.len()];

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

        info!(
            target: TARGET,
            "CONTINUOUS NEXT WALLET cycle {} B: Prepare transactions to next wallet concurrently",
            cycle + 1
        );
        let mut prepared = prepare_ring_send_round_send_with_utxo_cache(
            world,
            step,
            transactions_per_wallet,
            value,
            from,
            to,
            available_utxos,
            Some(policy.horizon.ceiling_prices.clone()),
            policy.priority_fee_percent,
        )
        .await?;
        prepared_counts.insert(from.clone(), prepared.len());
        validate_fee_horizon_after_wallet_batch(world, &policy, from, prepared.len()).await?;
        signed_submissions.append(&mut prepared);
    }

    let mut cycle_used_input_note_ids: HashSet<NoteId> = HashSet::new();
    for submission in &signed_submissions {
        extend_note_id_set(
            &mut cycle_used_input_note_ids,
            &submission.reserved_inputs().input_note_ids_list(),
        );
    }

    log_phase_counts(
        "CONTINUOUS NEXT WALLET",
        cycle,
        "C",
        "prepared",
        &prepared_counts,
    );

    let submitted_hashes = utils::submit_signed_user_wallet_submissions_concurrently(
        world,
        signed_submissions,
        Some(&policy),
    )
    .await?;
    let mut submitted_counts = BTreeMap::new();
    for (sender, _) in &submitted_hashes {
        *submitted_counts.entry(sender.clone()).or_insert(0usize) += 1;
    }
    log_phase_counts(
        "CONTINUOUS NEXT WALLET",
        cycle,
        "D",
        "submitted",
        &submitted_counts,
    );
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
