use super::{
    CucumberWorld, CustomRepublishDeps, DEFAULT_ZONE_SEQUENCER, DriveMode, Duration, HashSet,
    Inscription, PublishDeadline, Step, StepError, StepResult, VecDeque, custom_tx_rows,
    log_step_error, make_inscription, publish_message_with_retry, publish_zone_messages,
    remember_published_zone_message, start_named_sequencer, wait_for_channel_view,
    wait_for_indexer_unordered, when, zone_message_rows, zone_step_error,
};

#[when("I publish the following zone messages:")]
async fn step_publish_zone_messages(world: &mut CucumberWorld, step: &Step) -> StepResult {
    publish_zone_messages(
        world,
        step,
        DEFAULT_ZONE_SEQUENCER,
        zone_message_rows(step)?,
    )
    .await
}

#[when(expr = "sequencer {string} publishes the following zone messages:")]
async fn step_publish_zone_messages_for_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    publish_zone_messages(world, step, sequencer_alias, zone_message_rows(step)?).await
}

/// Publishing while the sequencer's node is down must be rejected: with
/// funding configured, building a transaction needs the node's wallet, so
/// the sequencer fails fast with `Unavailable` once the stream drop is
/// noticed (or surfaces the funding error in the brief window before). A
/// fresh `Ready` event fires once the node is back and a live block
/// confirms the reconnect.
#[cucumber::then(
    expr = "publishing zone message with data {string} via sequencer {string} fails while the node is down"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first `&mut` argument"
)]
async fn step_publish_fails_while_node_down(
    world: &mut CucumberWorld,
    step: &Step,
    data: String,
    sequencer_alias: String,
) -> StepResult {
    let _ = step;
    let payload = make_inscription(&data);
    let client = world.zone.sequencer_client(&sequencer_alias)?.clone();
    match client.publish(payload).await {
        Ok(_) => Err(StepError::LogicalError {
            message: format!(
                "Zone publish unexpectedly succeeded for sequencer '{sequencer_alias}' while its node is down"
            ),
        }),
        Err(_expected) => Ok(()),
    }
}

#[when(
    expr = "I submit zone message {string} to sequencer {string} with data {string} immediately"
)]
async fn step_publish_single_zone_message_for_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    message_alias: String,
    sequencer_alias: String,
    data: String,
) -> StepResult {
    let _ = step;
    let payload = make_inscription(&data);
    let handle = world.zone.sequencer_client(&sequencer_alias)?.clone();

    let (published, _checkpoint) = handle
        .publish(payload.clone())
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "Zone publish failed for sequencer '{sequencer_alias}' and message '{message_alias}': {error}"
            ),
        })?;

    remember_published_zone_message(world, &sequencer_alias, message_alias, payload, &published);

    Ok(())
}

#[when(
    expr = "sequencer {string} submits zone message {string} with data {string} to queue immediately"
)]
async fn step_publish_single_zone_message_to_queue_for_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    message_alias: String,
    data: String,
) -> StepResult {
    step_publish_single_zone_message_for_sequencer(
        world,
        step,
        message_alias,
        sequencer_alias,
        data,
    )
    .await
}

#[when(
    "the following custom transactions are published concurrently with custom republish policy:"
)]
async fn step_publish_custom_txs_concurrently_with_policy(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = custom_tx_rows(step)?;
    let mut expected_payloads = Vec::new();

    for row in &rows {
        let batches: VecDeque<Vec<Inscription>> = (0..row.transactions)
            .map(|tx_index| {
                (0..row.inscriptions)
                    .map(|entry_index| {
                        make_inscription(&format!(
                            "custom-{}-{tx_index}-{entry_index}",
                            row.sequencer_alias
                        ))
                    })
                    .collect()
            })
            .collect();
        expected_payloads.extend(batches.iter().flatten().cloned());

        let node_client = log_step_error(
            step,
            world.zone_node_http_client_for_sequencer(&row.sequencer_alias),
        )?;
        let node_name = world
            .zone
            .sequencer_node_name(&row.sequencer_alias)?
            .to_owned();
        let funding_pk = world.funding_wallet(&node_name)?.public_key()?;
        let deps = CustomRepublishDeps {
            node_client,
            channel_id: world.zone.sequencer_channel_id(&row.sequencer_alias)?,
            signing_key: world
                .zone
                .sequencer_signing_key(&row.sequencer_alias)?
                .clone(),
            funding_pk,
            batches,
        };

        start_named_sequencer(
            world,
            step,
            &row.sequencer_alias,
            None,
            DriveMode::CustomRepublish {
                deps: Box::new(deps),
            },
        )
        .await?;
    }

    world
        .zone
        .remember_expected_custom_payloads(expected_payloads);
    Ok(())
}

#[cucumber::then(expr = "the zone indexer returns all custom payloads in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_all_custom_payloads(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let expected: HashSet<Inscription> = world
        .zone
        .expected_custom_payloads()
        .iter()
        .cloned()
        .collect();
    if expected.is_empty() {
        return Err(StepError::LogicalError {
            message: "no custom transactions were planned".to_owned(),
        });
    }
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_indexer_unordered(indexer, &expected, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))?;
    Ok(())
}

#[when(
    expr = "I submit zone message {string} to sequencer {string} with data {string} on its turn"
)]
async fn step_publish_single_zone_message_for_sequencer_on_turn(
    world: &mut CucumberWorld,
    step: &Step,
    message_alias: String,
    sequencer_alias: String,
    data: String,
) -> StepResult {
    let payload = make_inscription(&data);
    let handle = world.zone.sequencer_client(&sequencer_alias)?.clone();
    let mut view_rx = world.zone.sequencer_channel_view_rx(&sequencer_alias)?;

    wait_for_channel_view(&mut view_rx, Duration::from_mins(3), |view| {
        view.our_turn_to_write
            && view.authorized_key_index.is_some()
            && view.authorized_key_index == view.own_key_index
    })
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    let published = publish_message_with_retry(
        &handle,
        &payload,
        PublishDeadline::from_now(Duration::from_mins(3)),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    remember_published_zone_message(world, &sequencer_alias, message_alias, payload, &published);

    Ok(())
}

#[when(expr = "sequencer {string} submits the following zone messages to queue immediately:")]
async fn step_publish_zone_messages_to_queue_for_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let rows = zone_message_rows(step)?;
    let handle = world.zone.sequencer_client(&sequencer_alias)?.clone();

    for (message_alias, payload) in rows {
        let (published, _checkpoint) = handle
            .publish(payload.clone())
            .await
            .map_err(|error| StepError::LogicalError {
                message: format!(
                    "Zone publish failed for sequencer '{sequencer_alias}' and message '{message_alias}': {error}"
                ),
            })?;

        remember_published_zone_message(
            world,
            &sequencer_alias,
            message_alias,
            payload,
            &published,
        );
    }

    Ok(())
}

/// Publish via [`SequencerClient`] and record each inscription id, without
/// waiting for on-chain inclusion.
///
/// Unlike `publishes the following zone messages` (which polls the node to
/// confirm inclusion), this performs no node HTTP calls, so it can be issued
/// while the node is down: each publish is accepted locally and posted on
/// reconnect. Recording the ids (the publish returns them locally) lets later
/// `... are finalized` / indexer assertions track the messages once the node is
/// back.
#[when(
    expr = "sequencer {string} submits the following zone messages without waiting for inclusion:"
)]
async fn step_publish_zone_messages_without_inclusion_for_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let rows = zone_message_rows(step)?;
    let handle = world.zone.sequencer_client(&sequencer_alias)?.clone();

    for (message_alias, payload) in rows {
        let (published, _checkpoint) = handle
            .publish(payload.clone())
            .await
            .map_err(|error| StepError::LogicalError {
                message: format!(
                    "Zone publish failed for sequencer '{sequencer_alias}' and message '{message_alias}': {error}"
                ),
            })?;
        remember_published_zone_message(
            world,
            &sequencer_alias,
            message_alias,
            payload,
            &published,
        );
    }

    Ok(())
}
