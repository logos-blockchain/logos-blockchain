use super::{
    BTreeMap, BuildHasher, CucumberWorld, Duration, GasPrices, HashSet, Instant, ManualCommand,
    NoteId, SignedUserWalletSubmission, StepError, TARGET, TransactionFeePolicy, TxHash,
    WalletSendReadiness, WalletUtxos, build_cycle_fee_policy, extend_note_id_set,
    extend_tx_hash_set, get_best_node_info, info, log_phase_counts,
    prepare_coin_splits_all_wallets_with_utxo_cache, sleep, sync, utils,
    validate_fee_horizon_after_wallet_batch, verify_transactions_mined,
    wait_for_observed_transaction_hashes, warn,
};

fn destructure_round_robin_command(
    command: &ManualCommand,
) -> Result<(usize, u64, usize, u64, usize, u32), StepError> {
    let ManualCommand::ContinuousRoundRobinUserWallets {
        coin_split_outputs,
        coin_split_value,
        num_transactions,
        value,
        cycles,
        epochs_headroom,
    } = command
    else {
        return Err(StepError::LogicalError {
            message: "expected ContinuousRoundRobinUserWallets command".to_owned(),
        });
    };
    Ok((
        *coin_split_outputs,
        *coin_split_value,
        *num_transactions,
        *value,
        *cycles,
        *epochs_headroom,
    ))
}

pub(super) fn all_user_wallets(world: &CucumberWorld) -> Result<Vec<String>, StepError> {
    let mut wallet_names = world
        .all_user_wallets()
        .iter()
        .map(|w| w.wallet_name.clone())
        .collect::<Vec<_>>();
    if wallet_names.len() < 2 {
        return Err(StepError::InvalidArgument {
            message: "This command requires at least two user wallets".to_owned(),
        });
    }
    wallet_names.sort();
    Ok(wallet_names)
}

#[expect(clippy::too_many_arguments, reason = "Transaction preparation inputs")]
async fn prepare_and_submit_round_robin_transactions(
    world: &mut CucumberWorld,
    step: &str,
    cycle: usize,
    wallet_names: &[String],
    num_transactions: usize,
    value: u64,
    available_utxos: &mut WalletUtxos,
    epochs_headroom: u32,
) -> Result<(HashSet<TxHash>, HashSet<NoteId>), StepError> {
    let policy = build_cycle_fee_policy(world, step, &wallet_names[0], epochs_headroom).await?;

    let mut signed_submissions = Vec::with_capacity(wallet_names.len() * num_transactions);
    let mut prepared_counts = BTreeMap::new();
    for sender in wallet_names {
        let recipients = recipient_wallets(wallet_names, sender)?;
        let mut prepared = prepare_round_robin_with_utxo_cache(
            world,
            step,
            sender,
            &recipients,
            num_transactions,
            value,
            available_utxos,
            Some(policy.horizon.ceiling_prices.clone()),
            policy.priority_fee_percent,
        )
        .await
        .map_err(|e| StepError::StepFail {
            message: format!(
                "CONTINUOUS ROUND ROBIN cycle {} failed to prepare transactions for sender \
                    '{sender}': {e}",
                cycle + 1,
            ),
        })?;

        prepared_counts.insert(sender.clone(), prepared.len());
        validate_fee_horizon_after_wallet_batch(world, &policy, sender, prepared.len()).await?;
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
        "CONTINUOUS ROUND ROBIN",
        cycle,
        "D",
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
        "CONTINUOUS ROUND ROBIN",
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

/// Manages the coin split process for a round robin cycle, including performing
/// the coin splits, waiting for them to be mined, and verifying that the
/// transactions were successfully mined. Returns a set of used input note IDs
/// from the coin split transactions. Note: This function needs a readiness
/// prepared UTXO cache.
#[expect(clippy::too_many_arguments, reason = "Round-robin split inputs")]
async fn manage_round_robin_coin_splits_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    cycle: usize,
    wallet_names: &[String],
    coin_split_outputs: usize,
    coin_split_value: u64,
    epochs_headroom: u32,
    available_utxos: &mut WalletUtxos,
) -> Result<HashSet<NoteId>, StepError> {
    let policy = build_cycle_fee_policy(world, step, &wallet_names[0], epochs_headroom).await?;

    let (split_tx_hashes, split_used_input_note_ids) =
        perform_coin_splits_for_round_robin_with_utxo_cache(
            world,
            step,
            wallet_names,
            coin_split_outputs,
            coin_split_value,
            cycle,
            available_utxos,
            &policy,
        )
        .await?;

    wait_for_n_blocks_or_warn(world, step, wallet_names, Duration::from_mins(3), 2, cycle).await?;

    verify_transactions_mined(
        world,
        step,
        &split_tx_hashes,
        wallet_names.len(),
        Some(cycle + 1),
        "CONTINUOUS ROUND ROBIN",
        "B",
    )
    .await?;

    Ok(split_used_input_note_ids)
}

async fn refresh_round_robin_sender_cache_entries<S: BuildHasher + Sync>(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    required_available: u64,
    readiness: WalletSendReadiness,
    available_utxos: &mut WalletUtxos,
    used_input_note_ids: &HashSet<NoteId, S>,
) -> Result<(), StepError> {
    for sender in wallet_names {
        sync::wait_wallet_send_ready(
            world,
            step,
            sender,
            180,
            required_available,
            readiness,
            available_utxos,
            used_input_note_ids,
        )
        .await?;
    }

    Ok(())
}

pub(super) fn verify_no_duplicate_transactions(
    cycle_tx_hashes: &HashSet<TxHash>,
    all_tx_hashes: &HashSet<TxHash>,
    cycle: usize,
    scenario_tag: &str,
) -> Result<(), StepError> {
    let duplicate_hashes = cycle_tx_hashes
        .intersection(all_tx_hashes)
        .copied()
        .collect::<Vec<_>>();
    if duplicate_hashes.is_empty() {
        Ok(())
    } else {
        Err(StepError::StepFail {
            message: format!(
                "{scenario_tag} cycle {} prepared/submitted {} duplicate transaction hash(es) from \
                previous cycles",
                cycle + 1,
                duplicate_hashes.len(),
            ),
        })
    }
}

#[expect(clippy::too_many_lines, reason = "Test function.")]
#[expect(
    clippy::cognitive_complexity,
    reason = "This function has multiple steps that are logically distinct."
)]
pub(super) async fn execute_continuous_round_robin(
    world: &mut CucumberWorld,
    step: &str,
    command: &ManualCommand,
) -> Result<(), StepError> {
    let (coin_split_outputs, coin_split_value, num_transactions, value, cycles, epochs_headroom) =
        destructure_round_robin_command(command)?;
    let wallet_names = all_user_wallets(world)?;

    let mut used_input_note_ids: HashSet<NoteId> = HashSet::new();
    let mut all_round_robin_tx_hashes = HashSet::new();
    let mut available_utxos = WalletUtxos::new();
    for cycle in 0..cycles {
        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} A: Wait for available coin-split funds all wallets",
            cycle + 1
        );

        refresh_round_robin_sender_cache_entries(
            world,
            step,
            &wallet_names,
            coin_split_outputs as u64 * coin_split_value,
            WalletSendReadiness::TotalValueOnly,
            &mut available_utxos,
            &used_input_note_ids,
        )
        .await?;

        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} B: Perform coin splits all wallets and wait mined",
            cycle + 1
        );

        let split_used_input_note_ids = manage_round_robin_coin_splits_with_utxo_cache(
            world,
            step,
            cycle,
            &wallet_names,
            coin_split_outputs,
            coin_split_value,
            epochs_headroom,
            &mut available_utxos,
        )
        .await?;
        extend_note_id_set(&mut used_input_note_ids, &split_used_input_note_ids);

        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} C: Wait all wallets ready with coin-split outputs",
            cycle + 1
        );

        refresh_round_robin_sender_cache_entries(
            world,
            step,
            &wallet_names,
            num_transactions as u64 * value,
            WalletSendReadiness::EligibleUtxoBatch {
                min_required_outputs: num_transactions,
                min_value_per_transaction: value,
            },
            &mut available_utxos,
            &used_input_note_ids,
        )
        .await?;

        info!(
            target: TARGET,
            "CONTINUOUS ROUND ROBIN cycle {} D: Prepare and send all round robin transactions",
            cycle + 1
        );

        let (cycle_tx_hashes, cycle_used_input_note_ids) =
            prepare_and_submit_round_robin_transactions(
                world,
                step,
                cycle,
                &wallet_names,
                num_transactions,
                value,
                &mut available_utxos,
                epochs_headroom,
            )
            .await?;
        verify_no_duplicate_transactions(
            &cycle_tx_hashes,
            &all_round_robin_tx_hashes,
            cycle,
            "CONTINUOUS ROUND ROBIN",
        )?;
        extend_note_id_set(&mut used_input_note_ids, &cycle_used_input_note_ids);

        // Assert transaction count
        verify_transactions_mined(
            world,
            step,
            &cycle_tx_hashes,
            wallet_names.len() * num_transactions,
            Some(cycle + 1),
            "CONTINUOUS ROUND ROBIN",
            "E",
        )
        .await?;

        // Collect hashes for final drain verification
        extend_tx_hash_set(&mut all_round_robin_tx_hashes, &cycle_tx_hashes);
    }

    // Final drain: verify submitted D-phase transaction hashes are observed in
    // chain blocks.
    info!(
        target: TARGET,
        "CONTINUOUS ROUND ROBIN final: Verify {} submitted round-robin transaction(s) were observed \
        in chain blocks",
        all_round_robin_tx_hashes.len(),
    );

    wait_for_observed_transaction_hashes(
        world,
        step,
        &all_round_robin_tx_hashes,
        Duration::from_mins(10),
    )
    .await?;

    info!(
        target: TARGET,
        "CONTINUOUS ROUND ROBIN scenario complete: {} transaction(s) verified from observed chain block transaction hashes across {} cycle(s)",
        all_round_robin_tx_hashes.len(),
        cycles
    );

    Ok(())
}

async fn wait_for_n_blocks_or_warn(
    world: &CucumberWorld,
    step: &str,
    wallet_names: &[String],
    time_out: Duration,
    blocks_to_wait: u64,
    cycle: usize,
) -> Result<(), StepError> {
    if wallet_names.is_empty() {
        return Err(StepError::InvalidArgument {
            message: "No wallet names provided for wait_for_n_blocks".to_owned(),
        });
    }
    let mut last_msg = String::new();
    let best_node_info = get_best_node_info(world, &wallet_names[0], Some(&mut last_msg)).await?;
    let node = world
        .resolve_node_http_client(&best_node_info.best_node_for_wallet(world, &wallet_names[0])?)?;
    let start_height = node.consensus_info().await?.cryptarchia_info.height;
    let start = Instant::now();
    loop {
        sleep(Duration::from_secs(1)).await;
        let best_node_info =
            get_best_node_info(world, &wallet_names[0], Some(&mut last_msg)).await?;
        let node = world.resolve_node_http_client(
            &best_node_info.best_node_for_wallet(world, &wallet_names[0])?,
        )?;
        let height = node.consensus_info().await?.cryptarchia_info.height;
        if height >= start_height + blocks_to_wait {
            return Ok(());
        }
        if start.elapsed() > time_out {
            warn!(
                target: TARGET,
                "Step `{step}` cycle {}: Chain could not grow by {blocks_to_wait} blocks in {time_out:.2?}",
                cycle + 1
            );
            return Ok(());
        }
    }
}

#[expect(clippy::too_many_arguments, reason = "Round-robin split inputs")]
async fn perform_coin_splits_for_round_robin_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    wallet_names: &[String],
    coin_split_outputs: usize,
    coin_split_value: u64,
    cycle: usize,
    available_utxos: &mut WalletUtxos,
    policy: &TransactionFeePolicy,
) -> Result<(HashSet<TxHash>, HashSet<NoteId>), StepError> {
    info!(target: TARGET, "CONTINUOUS ROUND ROBIN cycle {} B: Perform coin splits all wallets", cycle + 1);

    let (signed_submissions, prepared_split_counts) =
        prepare_coin_splits_all_wallets_with_utxo_cache(
            world,
            step,
            wallet_names,
            coin_split_outputs,
            coin_split_value,
            available_utxos,
            Some(policy.horizon.ceiling_prices.clone()),
            policy.priority_fee_percent,
        )
        .await?;
    log_phase_counts(
        "CONTINUOUS ROUND ROBIN",
        cycle,
        "B",
        "split prepared",
        &prepared_split_counts,
    );

    let mut split_used_input_note_ids: HashSet<NoteId> = HashSet::new();
    for submission in &signed_submissions {
        extend_note_id_set(
            &mut split_used_input_note_ids,
            &submission.reserved_inputs().input_note_ids_list(),
        );
    }

    let submitted_split_hashes = utils::submit_signed_user_wallet_submissions_concurrently(
        world,
        signed_submissions,
        Some(policy),
    )
    .await?;
    let mut submitted_split_counts = BTreeMap::new();
    for (sender, _) in &submitted_split_hashes {
        *submitted_split_counts
            .entry(sender.clone())
            .or_insert(0usize) += 1;
    }
    log_phase_counts(
        "CONTINUOUS ROUND ROBIN",
        cycle,
        "B",
        "split submitted",
        &submitted_split_counts,
    );

    let split_tx_hashes = submitted_split_hashes
        .into_iter()
        .map(|(_, tx_hash)| tx_hash)
        .collect::<HashSet<_>>();

    Ok((split_tx_hashes, split_used_input_note_ids))
}

fn recipient_wallets(wallet_names: &[String], sender: &str) -> Result<Vec<String>, StepError> {
    let recipients: Vec<_> = wallet_names
        .iter()
        .filter(|wallet| wallet.as_str() != sender)
        .cloned()
        .collect();
    if recipients.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("No recipient wallets available for sender '{sender}'"),
        });
    }

    Ok(recipients)
}

#[expect(clippy::too_many_arguments, reason = "Transaction preparation inputs")]
async fn prepare_round_robin_with_utxo_cache(
    world: &mut CucumberWorld,
    step: &str,
    sender: &str,
    recipients: &[String],
    transactions: usize,
    value: u64,
    available_utxos: &mut WalletUtxos,
    gas_prices: Option<GasPrices>,
    priority_fee_percent: u64,
) -> Result<Vec<SignedUserWalletSubmission>, StepError> {
    let mut reserved_submissions = Vec::with_capacity(transactions);

    for i in 0..transactions {
        let receiver_name = &recipients[i % recipients.len()];
        let receiver = world.resolve_recipient(receiver_name)?;
        let receiver_pk = receiver.public_key;

        let receivers = vec![(receiver_pk, value)];
        let reserved_submission =
            utils::reserve_user_wallet_transaction_submission_with_utxo_cache(
                world,
                step,
                sender,
                &receivers,
                available_utxos,
                gas_prices.clone(),
                priority_fee_percent,
            )
            .await?;

        reserved_submissions.push(reserved_submission);
    }

    utils::finalize_reserved_user_wallet_submissions_concurrently(step, reserved_submissions).await
}
