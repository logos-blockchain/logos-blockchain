use super::{
    AtomicZoneDepositRequest, CucumberWorld, Duration, Ed25519Key, Inscription, Keys, Metadata,
    PublishDeadline, PublishResult, SequencerCheckpoint, Step, StepError, StepResult, TxHash, Utxo,
    WalletInfo, WalletReservedInputs, ZONE_CHANNEL_DEPOSIT_THRESHOLD,
    ZONE_CHANNEL_WITHDRAW_THRESHOLD, ZoneDeposit, ZoneTestError, build_zone_deposit,
    build_zone_deposit_from_values, current_available_utxos_for_wallet, log_step_error,
    make_inscription, publish_atomic_zone_withdraw, submit_atomic_zone_deposit,
    submit_zone_channel_split, submit_zone_deposit, submit_zone_withdraw, timeout, zone_step_error,
};

pub(in super::super) async fn submit_zone_channel_config(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    authorized_aliases: Vec<String>,
    posting_timeframe: u32,
    posting_timeout: u32,
) -> StepResult {
    let handle = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
    let mut ordered_aliases = vec![sequencer_alias.to_owned()];

    for alias in authorized_aliases {
        if ordered_aliases.iter().any(|existing| existing == &alias) {
            continue;
        }

        ordered_aliases.push(alias);
    }

    let authorized_keys = ordered_aliases
        .into_iter()
        .map(|alias| {
            world
                .zone
                .sequencer_signing_key(&alias)
                .map(Ed25519Key::public_key)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut checkpoint_rx = world
        .zone
        .checkpoint_receiver(sequencer_alias)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Zone sequencer '{sequencer_alias}' has no checkpoint watch"),
        })?;
    checkpoint_rx.mark_unchanged();

    let ((result, post_call_checkpoint), _signed_tx) = handle
        .channel_config(
            Keys::new_unchecked(authorized_keys),
            posting_timeframe.into(),
            posting_timeout.into(),
            ZONE_CHANNEL_WITHDRAW_THRESHOLD,
            ZONE_CHANNEL_DEPOSIT_THRESHOLD,
        )
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Zone channel_config failed: {error}"),
        })?;

    // Sanity-check the inline checkpoint already mentions our tx; the
    // event-stream watcher below also catches it once the drive task
    // re-publishes its checkpoint after the next block.
    let tx_hash = result.inscription_id();
    let checkpoint = if post_call_checkpoint
        .pending_txs
        .iter()
        .any(|(hash, _)| *hash == tx_hash)
    {
        post_call_checkpoint
    } else {
        timeout(
            Duration::from_secs(30),
            wait_for_checkpoint_with_tx(&mut checkpoint_rx, tx_hash),
        )
        .await
        .map_err(|_| StepError::LogicalError {
            message: format!(
                "timed out waiting for sequencer '{sequencer_alias}' checkpoint to include {tx_hash:?}",
            ),
        })?
        .map_err(|message| StepError::LogicalError { message })?
    };

    world
        .zone
        .set_latest_checkpoint_for(sequencer_alias, checkpoint.clone());
    world
        .zone
        .remember_checkpoint(format!("{transaction_alias}_CHECKPOINT"), checkpoint);
    world.remember_submitted_transaction(transaction_alias, tx_hash);

    Ok(())
}

pub(in super::super) fn stop_zone_sequencer(
    world: &mut CucumberWorld,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    world.zone.stop_sequencer(sequencer_alias.as_ref())?;

    Ok(())
}

pub(in super::super) fn save_zone_checkpoint(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    checkpoint_alias: String,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref();
    let checkpoint = log_step_error(step, world.zone.current_checkpoint_for(sequencer_alias))?;

    world.zone.remember_checkpoint(checkpoint_alias, checkpoint);

    Ok(())
}

pub(in super::super) fn remember_published_zone_message(
    world: &mut CucumberWorld,
    sequencer_alias: &str,
    message_alias: String,
    payload: Inscription,
    result: &PublishResult,
) {
    let checkpoint = world.zone.current_checkpoint_for(sequencer_alias).ok();
    world.zone.remember_zone_message(
        message_alias,
        payload,
        Some(result.inscription_id()),
        Some(sequencer_alias),
        checkpoint,
    );
}

async fn wait_for_checkpoint_with_tx(
    rx: &mut tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    tx_hash: TxHash,
) -> Result<SequencerCheckpoint, String> {
    loop {
        let snapshot = rx.borrow_and_update().clone();
        if let Some(checkpoint) = snapshot
            && checkpoint
                .pending_txs
                .iter()
                .any(|(hash, _)| *hash == tx_hash)
        {
            return Ok(checkpoint);
        }

        rx.changed()
            .await
            .map_err(|error| format!("checkpoint watch closed: {error}"))?;
    }
}

fn resolve_zone_wallet(
    world: &CucumberWorld,
    sequencer_alias: &str,
) -> Result<WalletInfo, StepError> {
    let wallet_name = world.zone.sequencer_default_wallet_name(sequencer_alias)?;

    world.resolve_wallet(wallet_name)
}

fn record_zone_wallet_submission(
    world: &CucumberWorld,
    wallet_name: &str,
    tx_hash: TxHash,
    reserved_inputs: Vec<Utxo>,
) -> StepResult {
    world.with_wallets_mut(|wallets| {
        wallets.record_wallet_reservation(
            wallet_name.to_owned(),
            tx_hash,
            WalletReservedInputs::new(reserved_inputs, Vec::new()),
            0,
        );
    })
}

pub(in super::super) async fn submit_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    amount: u64,
    metadata: Metadata,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(&channel_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, &channel_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let available_utxos = log_step_error(
        step,
        current_available_utxos_for_wallet(world, &step.value, &wallet.wallet_name).await,
    )?;
    let ZoneDeposit {
        deposit,
        reserved_inputs,
        channel_notes,
    } = build_zone_deposit(
        available_utxos,
        world.zone.sequencer_channel_id(&channel_alias)?,
        amount,
        metadata,
    )
    .map_err(|error| zone_step_error(step, &error))?;

    let response = submit_zone_deposit(&node_url, &deposit, public_key)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_deposit_channel_notes(transaction_alias.clone(), channel_notes);
    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), deposit, amount);
    record_zone_wallet_submission(world, &wallet.wallet_name, response, reserved_inputs)?;
    world.remember_submitted_transaction(transaction_alias, response);

    Ok(())
}

/// Submits a multi-input channel deposit that consumes one wallet note per
/// listed value, exercising the channel wallet's per-note value tracking.
pub(in super::super) async fn submit_zone_multi_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    input_values: Vec<u64>,
    metadata: Metadata,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(&channel_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, &channel_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let available_utxos = log_step_error(
        step,
        current_available_utxos_for_wallet(world, &step.value, &wallet.wallet_name).await,
    )?;
    let amount: u64 = input_values.iter().sum();
    let ZoneDeposit {
        deposit,
        reserved_inputs,
        channel_notes,
    } = build_zone_deposit_from_values(
        available_utxos,
        world.zone.sequencer_channel_id(&channel_alias)?,
        &input_values,
        metadata,
    )
    .map_err(|error| zone_step_error(step, &error))?;

    let response = submit_zone_deposit(&node_url, &deposit, public_key)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_deposit_channel_notes(transaction_alias.clone(), channel_notes);
    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), deposit, amount);
    record_zone_wallet_submission(world, &wallet.wallet_name, response, reserved_inputs)?;
    world.remember_submitted_transaction(transaction_alias, response);

    Ok(())
}

pub(in super::super) async fn submit_zone_channel_split_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    deposit_alias: &str,
    dust_count: usize,
    transaction_alias: String,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(sequencer_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let channel_id = world.zone.sequencer_channel_id(sequencer_alias)?;
    let signing_key =
        log_step_error(step, world.zone.sequencer_signing_key(sequencer_alias))?.clone();
    let input_note = *log_step_error(
        step,
        world.zone.resolve_deposit_channel_notes(deposit_alias),
    )?
    .first()
    .ok_or_else(|| StepError::LogicalError {
        message: format!("Zone deposit '{deposit_alias}' created no channel notes to split"),
    })?;

    let tx_hash = submit_zone_channel_split(
        &node_url,
        channel_id,
        &signing_key,
        public_key,
        input_note,
        dust_count,
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world.remember_submitted_transaction(transaction_alias, tx_hash);

    Ok(())
}

pub(in super::super) async fn submit_atomic_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
    metadata: Metadata,
) -> StepResult {
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(sequencer_alias))?;
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let available_utxos = log_step_error(
        step,
        current_available_utxos_for_wallet(world, &step.value, &wallet.wallet_name).await,
    )?;
    let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Mint {amount} to Alice"));

    let submission = submit_atomic_zone_deposit(
        &node_url,
        sequencer,
        AtomicZoneDepositRequest {
            channel_id: world.zone.sequencer_channel_id(sequencer_alias)?,
            funding_public_key: public_key,
            available_utxos,
            amount,
            metadata,
            inscription_data: inscription_data.clone(),
        },
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_deposit(transaction_alias.clone(), submission.deposit, amount);
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    record_zone_wallet_submission(
        world,
        &wallet.wallet_name,
        submission.publish.inscription_id(),
        submission.reserved_inputs,
    )?;
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id());

    Ok(())
}

pub(in super::super) async fn submit_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
) -> StepResult {
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?;
    let inscription_data = make_inscription(&format!("Burn {amount}"));

    let submission = submit_zone_withdraw(
        sequencer,
        world.zone.sequencer_channel_id(sequencer_alias)?,
        public_key,
        amount,
        inscription_data.clone(),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .remember_submitted_withdraw(transaction_alias.clone(), submission.withdraw);
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    world.remember_submitted_transaction(transaction_alias, submission.publish.inscription_id());

    Ok(())
}

/// Action wrapper for the new `publish_atomic_withdraw` SDK API. Mirrors
/// [`submit_zone_withdraw_transaction`] but uses the high-level fire-and-forget
/// flow: SDK fills the withdraw nonce, locates its own accredited-key index,
/// builds the bundled `MantleTx`, signs locally, and submits.
///
/// `withdraw_rows` carries one `(alias, outputs)` per `WithdrawArg`; each
/// withdraw is remembered under its own alias so multi-withdraw bundles can
/// be asserted per-withdraw via the indexer step. `bundle_alias` is remembered
/// as the bundle's tx hash for `zone transaction "..." is finalized`.
pub(in super::super) async fn publish_atomic_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    bundle_alias: String,
    message_alias: String,
    withdraw_rows: Vec<(String, Vec<u64>)>,
) -> StepResult {
    let wallet = log_step_error(step, resolve_zone_wallet(world, sequencer_alias))?;
    let public_key = log_step_error(step, wallet.public_key())?;
    let total: u64 = withdraw_rows
        .iter()
        .flat_map(|(_, outputs)| outputs.iter())
        .sum();
    let inscription_data = make_inscription(&format!("Burn {total}"));
    let outputs_per_arg: Vec<Vec<u64>> = withdraw_rows
        .iter()
        .map(|(_, outputs)| outputs.clone())
        .collect();

    let submission = {
        let sequencer = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?.clone();

        publish_atomic_zone_withdraw(
            &sequencer,
            public_key,
            outputs_per_arg,
            inscription_data.clone(),
            PublishDeadline::from_now(Duration::from_mins(3)),
        )
        .await
        .map_err(|error| zone_step_error(step, &error))?
    };

    // A bundle carries a single `ChannelWithdrawOp` that releases every
    // recipient note the transfer created, regardless of how many withdraw args
    // were passed. Remember that one op under each row alias so per-withdraw
    // indexer assertions all resolve to the same finalized op.
    let [withdraw_op] = submission.withdraws.as_slice() else {
        return Err(zone_step_error(
            step,
            &ZoneTestError::SubmitWithdraw {
                message: format!(
                    "atomic withdraw bundle produced {} withdraw ops, expected exactly 1",
                    submission.withdraws.len(),
                ),
            },
        ));
    };
    for (alias, _) in &withdraw_rows {
        world
            .zone
            .remember_submitted_withdraw(alias.clone(), withdraw_op.clone());
    }
    remember_published_zone_message(
        world,
        sequencer_alias,
        message_alias,
        inscription_data,
        &submission.publish,
    );
    world.remember_submitted_transaction(bundle_alias, submission.publish.inscription_id());

    Ok(())
}
