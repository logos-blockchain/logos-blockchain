use super::{
    ConcurrentZoneMessageRow, CucumberWorld, DEFAULT_ZONE_SEQUENCER, Duration, Inscription,
    PublishDeadline, PublishedZoneMessage, Step, StepError, StepResult, ZoneReaderConfig,
    ensure_zone_transactions_included, group_zone_messages_by_sequencer, join_all, log_step_error,
    publish_message_with_retry, remember_published_zone_message, zone_step_error,
};

pub(in super::super) fn initialize_zone_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref();
    let node_url = log_step_error(step, world.zone_node_url_for_sequencer(sequencer_alias))?;
    let indexer = ZoneReaderConfig {
        channel_id: world.zone.sequencer_channel_id(sequencer_alias)?,
        node_url,
    };

    world.zone.set_indexer(indexer);

    Ok(())
}

pub(in super::super) async fn publish_zone_messages(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: impl AsRef<str>,
    rows: Vec<(String, Inscription)>,
) -> StepResult {
    let sequencer_alias = sequencer_alias.as_ref().to_owned();
    let node = log_step_error(
        step,
        world.zone_node_http_client_for_sequencer(&sequencer_alias),
    )?;

    let published = {
        let sequencer =
            log_step_error(step, world.zone.sequencer_client(&sequencer_alias))?.clone();

        let publish_deadline = PublishDeadline::from_now(Duration::from_mins(3));
        let mut published = Vec::with_capacity(rows.len());

        for (alias, payload) in &rows {
            let result = publish_message_with_retry(&sequencer, payload, publish_deadline)
                .await
                .map_err(|error| zone_step_error(step, &error))?;

            ensure_zone_transactions_included(
                &node,
                &[result.inscription_id()],
                Duration::from_mins(3),
            )
            .await
            .map_err(|error| zone_step_error(step, &error))?;

            published.push(PublishedZoneMessage {
                alias: alias.clone(),
                payload: payload.clone(),
                result,
            });
        }

        published
    };

    for message in published {
        remember_published_zone_message(
            world,
            &sequencer_alias,
            message.alias,
            message.payload,
            &message.result,
        );
    }

    Ok(())
}

pub(in super::super) async fn publish_zone_messages_concurrently(
    world: &mut CucumberWorld,
    step: &Step,
    rows: Vec<ConcurrentZoneMessageRow>,
) -> StepResult {
    let grouped = group_zone_messages_by_sequencer(&rows);
    let handles = grouped
        .keys()
        .map(|sequencer_alias| {
            log_step_error(step, world.zone.sequencer_client(sequencer_alias))
                .map(|handle| (sequencer_alias.clone(), handle.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;

    join_all(handles.into_iter().map(|(sequencer_alias, handle)| {
        let payloads = grouped[&sequencer_alias]
            .iter()
            .map(|message| message.payload.clone())
            .collect::<Vec<_>>();

        async move {
            for payload in payloads {
                handle.publish(payload).await.map_err(|error| {
                    StepError::LogicalError {
                        message: format!(
                            "Zone concurrent publish failed for sequencer '{sequencer_alias}': {error}"
                        ),
                    }
                })?;
            }

            Ok::<(), StepError>(())
        }
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    for row in rows {
        world
            .zone
            .remember_zone_message(row.message_alias, row.payload, None, None, None);
    }

    if world.zone.indexer().is_err() {
        initialize_zone_indexer(world, step, DEFAULT_ZONE_SEQUENCER)?;
    }

    Ok(())
}
