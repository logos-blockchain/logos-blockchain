use super::{
    CucumberWorld, Duration, Step, StepError, StepResult, log_step_error, single_column_table,
    wait_for_channel_view, wait_for_on_chain_statuses_and_collect_mempool_pending,
    wait_for_turn_to_write, zone_sequencing_state_row, zone_step_error,
};

#[cucumber::then(
    expr = "sequencer {string} reaches sequencing state OWN_KEY_INDEX {int} NOT_OUR_TURN with {int} pending publish txs in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_sequencer_reaches_sequencing_state_not_our_turn(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    own_key_index: usize,
    pending_publish_txs: usize,
    timeout_seconds: u64,
) -> StepResult {
    wait_for_sequencing_state(
        world,
        step,
        &sequencer_alias,
        own_key_index,
        false,
        pending_publish_txs,
        timeout_seconds,
    )
    .await
}

#[cucumber::then(
    expr = "sequencer {string} reaches sequencing state OWN_KEY_INDEX {int} OUR_TURN with {int} pending publish txs in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_sequencer_reaches_sequencing_state_our_turn(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    own_key_index: usize,
    pending_publish_txs: usize,
    timeout_seconds: u64,
) -> StepResult {
    wait_for_sequencing_state(
        world,
        step,
        &sequencer_alias,
        own_key_index,
        true,
        pending_publish_txs,
        timeout_seconds,
    )
    .await
}

#[cucumber::then(expr = "sequencer {string} reaches sequencing state:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_sequencer_reaches_sequencing_state_from_table(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let row = zone_sequencing_state_row(step)?;

    wait_for_sequencing_state(
        world,
        step,
        &sequencer_alias,
        row.own_key_index,
        row.is_our_turn,
        row.pending_transactions,
        row.timeout_seconds,
    )
    .await
}

async fn wait_for_sequencing_state(
    world: &CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    own_key_index: usize,
    is_our_turn: bool,
    pending_publish_txs: usize,
    timeout_seconds: u64,
) -> StepResult {
    let _handle = log_step_error(step, world.zone.sequencer_client(sequencer_alias))?.clone();
    let mut view_rx = log_step_error(step, world.zone.sequencer_channel_view_rx(sequencer_alias))?;

    wait_for_channel_view(
        &mut view_rx,
        Duration::from_secs(timeout_seconds),
        move |view| {
            view.own_key_index == Some(own_key_index as u16)
                && view.authorized_key_index.is_some()
                && view.our_turn_to_write == is_our_turn
                && (is_our_turn || view.authorized_key_index != view.own_key_index)
                && (!is_our_turn || view.authorized_key_index == Some(own_key_index as u16))
                && view.pending_publish_txs == pending_publish_txs
        },
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    Ok(())
}

#[cucumber::then(
    expr = "sequencer {string} is notified it is their turn to write in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_sequencer_notified_turn_to_write(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let mut turn_rx = log_step_error(
        step,
        world.zone.sequencer_turn_to_write_rx(&sequencer_alias),
    )?;
    wait_for_turn_to_write(&mut turn_rx, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    Ok(())
}

#[cucumber::then(
    expr = "sequencer {string} emits published events for queued zone messages on its turn in {int} seconds:"
)]
async fn step_sequencer_emits_published_events_for_queued_zone_messages_on_turn(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let tx_hashes = log_step_error(step, world.zone.message_tx_hashes_for_aliases(&aliases))?;
    let mut view_rx = log_step_error(step, world.zone.sequencer_channel_view_rx(&sequencer_alias))?;
    wait_for_channel_view(&mut view_rx, Duration::from_secs(timeout_seconds), |view| {
        view.our_turn_to_write
    })
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    // The messages were remembered when queued; wait until they're mined
    // (`OnChain`, not yet finalized) via the per-tx status stream.
    let mut statuses = log_step_error(
        step,
        world.zone.take_sequencer_tx_status_rx(&sequencer_alias),
    )?;
    let mempool_pending = wait_for_on_chain_statuses_and_collect_mempool_pending(
        &mut statuses,
        &tx_hashes,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .record_mempool_pending(sequencer_alias.clone(), mempool_pending);

    Ok(())
}

#[cucumber::then(expr = "sequencer {string} observed mempool pending events for zone messages:")]
#[expect(
    clippy::unused_async,
    reason = "Cucumber step functions are async even when assertion is synchronous"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_sequencer_emitted_mempool_pending_events_for_zone_messages(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let tx_hashes = log_step_error(step, world.zone.message_tx_hashes_for_aliases(&aliases))?;

    for (alias, tx_hash) in aliases.iter().zip(tx_hashes.iter()) {
        if !world
            .zone
            .has_observed_mempool_pending(&sequencer_alias, tx_hash)
        {
            return Err(StepError::LogicalError {
                message: format!(
                    "Sequencer '{sequencer_alias}' did not emit mempool pending event for zone message '{alias}'"
                ),
            });
        }
    }

    Ok(())
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
#[cucumber::then(expr = "sequencer {string} has {int} pending publish txs in {int} seconds")]
async fn step_sequencer_has_pending_publish_txs(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    pending_publish_txs: usize,
    timeout_seconds: u64,
) -> StepResult {
    let mut view_rx = log_step_error(step, world.zone.sequencer_channel_view_rx(&sequencer_alias))?;

    wait_for_channel_view(
        &mut view_rx,
        Duration::from_secs(timeout_seconds),
        move |view| view.pending_publish_txs == pending_publish_txs,
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    Ok(())
}

#[cucumber::then(
    expr = "sequencer {string} publishes {string} immediately while in turn in {int} seconds"
)]
async fn step_sequencer_publishes_immediately_while_in_turn(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    message_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    // The message was remembered when submitted; wait until it's mined
    // (`OnChain`, not yet finalized) via the per-tx status stream.
    let tx_hashes = log_step_error(
        step,
        world
            .zone
            .message_tx_hashes_for_aliases(std::slice::from_ref(&message_alias)),
    )?;
    let mut statuses = log_step_error(
        step,
        world.zone.take_sequencer_tx_status_rx(&sequencer_alias),
    )?;
    let mempool_pending = wait_for_on_chain_statuses_and_collect_mempool_pending(
        &mut statuses,
        &tx_hashes,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    world
        .zone
        .record_mempool_pending(sequencer_alias.clone(), mempool_pending);

    Ok(())
}
