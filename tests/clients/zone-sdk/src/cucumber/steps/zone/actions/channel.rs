use super::{
    Duration, Ed25519Key, Inscription, PublishResult, SequencerCheckpoint, Step, StepError,
    StepResult, TxHash, VerifiedChannelKeys, ZONE_CHANNEL_WITHDRAW_THRESHOLD, ZoneWorld,
    log_step_error, timeout,
};

pub(in super::super) async fn submit_zone_channel_config(
    world: &mut ZoneWorld,
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
            VerifiedChannelKeys::new_unchecked(authorized_keys),
            posting_timeframe.into(),
            posting_timeout.into(),
            ZONE_CHANNEL_WITHDRAW_THRESHOLD,
        )
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Zone channel_config failed: {error}"),
        })?;

    record_submitted_config(
        world,
        sequencer_alias,
        transaction_alias,
        &result,
        post_call_checkpoint,
        &mut checkpoint_rx,
    )
    .await
}

/// `threshold` sets the config threshold;
/// `posting_timeframe` is in slots (0 = no rotation).
pub(in super::super) async fn prepare_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
    authorized_aliases: Vec<String>,
    threshold: u16,
    posting_timeframe: u32,
) -> StepResult {
    let client = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?.clone();

    let authorized_keys = authorized_aliases
        .iter()
        .map(|alias| world.zone.sequencer_public_key(alias))
        .collect::<Result<Vec<_>, _>>()?;

    let prepared = client
        .prepare_channel_config(
            VerifiedChannelKeys::new_unchecked(authorized_keys.clone()),
            posting_timeframe.into(),
            0.into(),
            threshold,
        )
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Zone prepare_channel_config failed: {error}"),
        })?;

    // Read the funded config back the way a remote signer would (via
    // `proposed_config`) and confirm the SDK encoded exactly the key set and
    // thresholds we requested — i.e. inspect the config before it is signed.
    let proposed = prepared.proposed_config();
    let proposed_keys = proposed.keys().iter().copied().collect::<Vec<_>>();
    if proposed_keys != authorized_keys || proposed.configuration_threshold().get() != threshold {
        return Err(StepError::LogicalError {
            message: format!(
                "prepared config '{transaction_alias}' does not match the requested keys/thresholds",
            ),
        });
    }

    world
        .zone
        .remember_prepared_config(transaction_alias, prepared);

    Ok(())
}

/// Signs using only `signer_alias`'s own key.
pub(in super::super) fn sign_prepared_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    signer_alias: &str,
    transaction_alias: String,
) -> StepResult {
    let signing_key = log_step_error(step, world.zone.sequencer_signing_key(signer_alias))?;
    let prepared = log_step_error(step, world.zone.prepared_config(&transaction_alias))?;

    // `prepare_zone_channel_config` already verified `proposed_config()` against
    // the requested key set; a real remote signer would run that same check
    // here before signing rather than trusting the preparer.
    let signature = prepared
        .sign_with(signing_key)
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "signing prepared config '{transaction_alias}' as '{signer_alias}' failed: {error}",
            ),
        })?;

    world
        .zone
        .add_prepared_config_signature(transaction_alias, signature);

    Ok(())
}

pub(in super::super) async fn submit_prepared_zone_channel_config(
    world: &mut ZoneWorld,
    step: &Step,
    sequencer_alias: &str,
    transaction_alias: String,
) -> StepResult {
    let client = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?.clone();
    let prepared = log_step_error(step, world.zone.prepared_config(&transaction_alias))?.clone();
    // Collected in arbitrary signer order, which the proof's binary tree-based map
    // puts in index order.
    let signatures = world.zone.prepared_config_signatures(&transaction_alias);

    let mut checkpoint_rx = world
        .zone
        .checkpoint_receiver(sequencer_alias)
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Zone sequencer '{sequencer_alias}' has no checkpoint watch"),
        })?;
    checkpoint_rx.mark_unchanged();

    let (result, post_call_checkpoint) = client
        .submit_channel_config(prepared, signatures)
        .await
        .map_err(|error| StepError::LogicalError {
        message: format!("Zone submit_channel_config failed: {error}"),
    })?;

    record_submitted_config(
        world,
        sequencer_alias,
        transaction_alias,
        &result,
        post_call_checkpoint,
        &mut checkpoint_rx,
    )
    .await
}

async fn record_submitted_config(
    world: &mut ZoneWorld,
    sequencer_alias: &str,
    transaction_alias: String,
    result: &PublishResult,
    post_call_checkpoint: SequencerCheckpoint,
    checkpoint_rx: &mut tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
) -> StepResult {
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
            wait_for_checkpoint_with_tx(checkpoint_rx, tx_hash),
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
    world
        .node
        .remember_submitted_transaction(transaction_alias, tx_hash);

    Ok(())
}

pub(in super::super) fn stop_zone_sequencer(
    world: &mut ZoneWorld,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    world.zone.stop_sequencer(sequencer_alias.as_ref())?;

    Ok(())
}

pub(in super::super) fn save_zone_checkpoint(
    world: &mut ZoneWorld,
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
    world: &mut ZoneWorld,
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
        Some(result.tx.inscription().this_msg),
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
