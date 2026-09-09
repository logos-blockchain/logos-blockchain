use super::{
    Arc, ConcurrentZoneMessageRow, CucumberWorld, DEFAULT_ZONE_SEQUENCER, DriveMode,
    GeneratedZoneMessageBatch, HashMap, HashSet, Inscription, Step, StepResult,
    balance_update_payload, concurrent_zone_message_rows, generated_zone_message_batches,
    generated_zone_message_sequencers, group_zone_messages_by_sequencer, initialize_zone_indexer,
    make_inscription, publish_zone_messages_concurrently, start_named_sequencer,
    start_named_sequencer_with_pending_submit_depth, when, zone_balance_rows,
};

#[when("the following zone messages are published concurrently with republish policy:")]
async fn step_publish_zone_messages_concurrently_with_republish_policy(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = concurrent_zone_message_rows(step)?;
    publish_zone_messages_with_republish_policy(world, step, rows).await
}

#[when(
    expr = "each listed zone sequencer publishes {int} generated zone messages concurrently with republish policy:"
)]
async fn step_publish_generated_zone_messages_concurrently_with_republish_policy(
    world: &mut CucumberWorld,
    step: &Step,
    messages_per_sequencer: usize,
) -> StepResult {
    let rows = build_generated_zone_message_rows(
        generated_zone_message_batches(step)?,
        messages_per_sequencer,
    );

    publish_zone_messages_with_republish_policy(world, step, rows).await
}

#[when(
    expr = "each listed zone sequencer publishes {int} copies of zone message {string} concurrently with republish policy:"
)]
async fn step_publish_repeated_zone_messages_concurrently_with_republish_policy(
    world: &mut CucumberWorld,
    step: &Step,
    copies_per_sequencer: usize,
    payload: String,
) -> StepResult {
    let rows = build_repeated_zone_message_rows(
        generated_zone_message_sequencers(step)?,
        copies_per_sequencer,
        &payload,
    );

    publish_zone_messages_with_republish_policy(world, step, rows).await
}

fn build_generated_zone_message_rows(
    batches: Vec<GeneratedZoneMessageBatch>,
    messages_per_sequencer: usize,
) -> Vec<ConcurrentZoneMessageRow> {
    let mut builder = GeneratedZoneMessages::default();

    for batch in batches {
        builder.append_numbered_payloads(batch, messages_per_sequencer);
    }

    builder.finish()
}

fn build_repeated_zone_message_rows(
    sequencer_aliases: Vec<String>,
    copies_per_sequencer: usize,
    payload: &str,
) -> Vec<ConcurrentZoneMessageRow> {
    let mut builder = GeneratedZoneMessages::default();
    let payload = make_inscription(payload);

    for sequencer_alias in sequencer_aliases {
        builder.append_repeated_payloads(sequencer_alias, copies_per_sequencer, &payload);
    }

    builder.finish()
}

#[derive(Default)]
struct GeneratedZoneMessages {
    next_message_number: usize,
    rows: Vec<ConcurrentZoneMessageRow>,
}

impl GeneratedZoneMessages {
    fn append_numbered_payloads(&mut self, batch: GeneratedZoneMessageBatch, count: usize) {
        let GeneratedZoneMessageBatch {
            sequencer_alias,
            data_prefix,
        } = batch;

        for payload_number in 1..=count {
            self.push(
                sequencer_alias.clone(),
                make_inscription(&format!("{data_prefix}{payload_number}")),
            );
        }
    }

    fn append_repeated_payloads(
        &mut self,
        sequencer_alias: String,
        count: usize,
        payload: &Inscription,
    ) {
        for _ in 1..count {
            self.push(sequencer_alias.clone(), payload.clone());
        }

        if count > 0 {
            self.push(sequencer_alias, payload.clone());
        }
    }

    fn push(&mut self, sequencer_alias: String, payload: Inscription) {
        self.next_message_number += 1;

        self.rows.push(ConcurrentZoneMessageRow {
            sequencer_alias,
            message_alias: format!("MSG_{}", self.next_message_number),
            payload,
        });
    }

    fn finish(self) -> Vec<ConcurrentZoneMessageRow> {
        self.rows
    }
}

async fn publish_zone_messages_with_republish_policy(
    world: &mut CucumberWorld,
    step: &Step,
    rows: Vec<ConcurrentZoneMessageRow>,
) -> StepResult {
    let grouped = group_zone_messages_by_sequencer(&rows);

    for (sequencer_alias, messages) in &grouped {
        let planned = messages
            .iter()
            .map(|message| message.payload.clone())
            .collect();
        start_named_sequencer_with_pending_submit_depth(
            world,
            step,
            sequencer_alias,
            None,
            DriveMode::RepublishLineage { planned },
            usize::MAX,
        )
        .await?;
    }

    // Each sequencer's lineage policy owns publishing its own copies; the step
    // only records the messages so the indexer assertions can find them.
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

#[when("the following zone messages are published concurrently with sorted conflict policy:")]
async fn step_publish_zone_messages_concurrently_with_sorted_policy(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = concurrent_zone_message_rows(step)?;
    let grouped = group_zone_messages_by_sequencer(&rows);
    let discarded = Arc::new(tokio::sync::Mutex::new(HashSet::new()));

    for sequencer_alias in grouped.keys() {
        start_named_sequencer(
            world,
            step,
            sequencer_alias,
            None,
            DriveMode::Sorted {
                discarded: Arc::clone(&discarded),
            },
        )
        .await?;
    }

    world.zone.set_sorted_total_payloads(rows.len());
    world.zone.set_sorted_expected_by_sequencer(
        grouped
            .iter()
            .map(|(sequencer_alias, messages)| {
                (
                    sequencer_alias.clone(),
                    messages
                        .iter()
                        .map(|message| message.payload.clone())
                        .collect(),
                )
            })
            .collect(),
    );

    publish_zone_messages_concurrently(world, step, rows).await
}

#[when("the following zone balance updates are published concurrently with balance-aware policy:")]
async fn step_publish_zone_balance_updates_with_balance_policy(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = zone_balance_rows(step)?;
    let initial_balances = world.zone.zone_account_balances()?;
    let grouped = rows.iter().fold(
        HashMap::<String, Vec<(String, Inscription)>>::new(),
        |mut grouped, row| {
            let payload = balance_update_payload(&row.message_alias, &row.account, row.delta);
            grouped
                .entry(row.sequencer_alias.clone())
                .or_default()
                .push((row.message_alias.clone(), payload));
            grouped
        },
    );

    for (sequencer_alias, planned) in &grouped {
        start_named_sequencer(
            world,
            step,
            sequencer_alias,
            None,
            DriveMode::BalanceAware {
                initial_balances: initial_balances.clone(),
                planned_payloads: planned.iter().map(|(_, payload)| payload.clone()).collect(),
            },
        )
        .await?;
    }

    for messages in grouped.values() {
        for (message_alias, payload) in messages {
            world.zone.remember_zone_message(
                message_alias.clone(),
                payload.clone(),
                None,
                None,
                None,
            );
        }
    }

    if world.zone.indexer().is_err() {
        initialize_zone_indexer(world, step, DEFAULT_ZONE_SEQUENCER)?;
    }

    Ok(())
}
