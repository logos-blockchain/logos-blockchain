use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use cucumber::{gherkin::Step, given, when};
use lb_core::mantle::ops::channel::inscribe::Inscription;

use super::{
    actions::{
        DriveMode, initialize_zone_indexer, publish_atomic_zone_withdraw_transaction,
        publish_zone_messages, publish_zone_messages_concurrently,
        register_zone_sequencers_with_shared_key, remember_published_zone_message,
        save_zone_checkpoint, start_named_sequencer,
        start_named_sequencer_with_pending_submit_depth, start_nodes_with_zone_resources,
        stop_zone_sequencer, submit_atomic_zone_deposit_transaction, submit_zone_channel_config,
        submit_zone_deposit_transaction, submit_zone_withdraw_transaction,
    },
    assertions::{
        assert_sorted_outcome, scan_indexer_for_payloads, wait_for_indexer_unordered,
        wait_until_sorted_conflict_settles,
    },
    errors::{log_step_error, zone_step_error},
    runner::{TxSource, TxStatus},
    support::{
        PublishDeadline, balance_update_payload, collect_indexed_messages,
        collect_indexed_messages_exactly_once, ensure_zone_transactions_included,
        parse_balance_payload, publish_message_with_retry,
        wait_for_adopted_payload_and_collect_mempool_pending,
        wait_for_adopted_payloads_and_collect_mempool_pending, wait_for_channel_view,
        wait_for_deposit, wait_for_exact_indexed_payload_count,
        wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending,
        wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending,
        wait_for_lib_advance, wait_for_transactions_finalized, wait_for_turn_to_write,
        wait_for_tx_status_lifecycle, wait_for_withdraw,
    },
    tables::{
        ConcurrentZoneMessageRow, GeneratedZoneMessageBatch, concurrent_zone_message_rows,
        generated_zone_message_batches, generated_zone_message_sequencers,
        group_zone_messages_by_sequencer, single_column_table, zone_account_balances,
        zone_atomic_withdraw_rows, zone_balance_rows, zone_config_row, zone_message_rows,
        zone_node_resource_rows, zone_sequencer_start_rows, zone_sequencing_state_row,
    },
};
use crate::{
    common::mantle_inscription::make_inscription,
    cucumber::{
        error::{StepError, StepResult},
        world::{CucumberWorld, ZoneSequencerStartup},
    },
};

pub(super) const DEFAULT_ZONE_SEQUENCER: &str = "SEQ_A";

fn parse_submit_depth(step: &Step, value: &str) -> Result<usize, StepError> {
    let value = value.trim();
    if matches!(value.to_lowercase().as_str(), "unlimited" | "none") {
        return Ok(usize::MAX);
    }

    let inner = value
        .strip_prefix("Some(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(value);

    let limit = inner
        .parse::<usize>()
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "Invalid pending submit depth '{value}' in step '{}': {error}",
                step.value
            ),
        })?;

    Ok(limit)
}

fn parse_optional_submit_depth(step: &Step, value: &str) -> Result<Option<usize>, StepError> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("default") {
        return Ok(None);
    }

    parse_submit_depth(step, value).map(Some)
}

const fn passive_mode_for_startup(startup: ZoneSequencerStartup) -> DriveMode {
    if startup.passive_republish_orphans {
        DriveMode::passive_republish_orphans()
    } else {
        DriveMode::passive()
    }
}

async fn start_named_sequencer_with_startup(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
    checkpoint: Option<lb_zone_sdk::sequencer::SequencerCheckpoint>,
    startup: ZoneSequencerStartup,
) -> StepResult {
    let mode = passive_mode_for_startup(startup);
    if let Some(submit_depth) = startup.pending_submit_depth {
        start_named_sequencer_with_pending_submit_depth(
            world,
            step,
            sequencer_alias,
            checkpoint,
            mode,
            submit_depth,
        )
        .await
    } else {
        start_named_sequencer(world, step, sequencer_alias, checkpoint, mode).await
    }
}
const CONCURRENT_DUPLICATE_SETTLE_SECS: u64 = 30;

#[given("I start nodes with wallet and sequencer resources:")]
#[when("I start nodes with wallet and sequencer resources:")]
async fn step_start_nodes_with_wallet_and_sequencer_resources(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = zone_node_resource_rows(step)?;

    start_nodes_with_zone_resources(world, step, rows).await
}

#[given(expr = "the following zone sequencers share the signing key of {string}:")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Cucumber string captures are provided as owned `String`s"
)]
fn step_zone_sequencers_share_signing_key(
    world: &mut CucumberWorld,
    step: &Step,
    source_alias: String,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone sequencer aliases")?;
    register_zone_sequencers_with_shared_key(world, &source_alias, aliases)
}

#[given("the following zone account balances exist:")]
fn step_zone_account_balances(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let balances = zone_account_balances(step)?
        .into_iter()
        .map(|row| (row.account, row.balance))
        .collect();

    world.zone.set_zone_account_balances(balances);

    Ok(())
}

#[when(expr = "I start zone sequencer {string}")]
async fn step_start_zone_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::passive()).await
}

#[when(expr = "I start zone sequencer {string} with indexer")]
async fn step_start_zone_sequencer_with_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_sequencer_with_indexer(world, step, &sequencer_alias).await
}

async fn start_sequencer_with_indexer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: &str,
) -> StepResult {
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::passive()).await?;
    initialize_zone_indexer(world, step, sequencer_alias)
}

#[when("I start zone sequencers:")]
async fn step_start_zone_sequencers(world: &mut CucumberWorld, step: &Step) -> StepResult {
    for row in zone_sequencer_start_rows(step)? {
        let alias = row.alias;
        let startup = ZoneSequencerStartup {
            pending_submit_depth: parse_optional_submit_depth(step, &row.pending_submit_depth)?,
            passive_republish_orphans: row.passive_republish_orphans,
        };
        world.zone.set_sequencer_startup(&alias, startup);

        start_named_sequencer_with_startup(world, step, &alias, None, startup).await?;

        if row.indexer {
            initialize_zone_indexer(world, step, &alias)?;
        }
    }

    Ok(())
}

#[when(expr = "I stop zone sequencer {string}")]
fn step_stop_zone_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let _ = step;
    stop_zone_sequencer(world, sequencer_alias)
}

#[cucumber::when(expr = "the zone LIB advances in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_lib_advances(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone_node_http_client())?;
    let initial_lib_slot = client
        .consensus_info()
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!("Failed to fetch zone consensus info: {error}"),
        })?
        .cryptarchia_info
        .lib_slot;

    wait_for_lib_advance(
        &client,
        initial_lib_slot,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

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

    handle
        .publish(payload.clone())
        .await
        .map_err(|error| StepError::LogicalError {
            message: format!(
                "Zone publish failed for sequencer '{sequencer_alias}' and message '{message_alias}': {error}"
            ),
        })?;

    world
        .zone
        .remember_zone_message(message_alias, payload, None, None, None);

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
        handle
            .publish(payload.clone())
            .await
            .map_err(|error| StepError::LogicalError {
                message: format!(
                    "Zone publish failed for sequencer '{sequencer_alias}' and message '{message_alias}': {error}"
                ),
            })?;

        world
            .zone
            .remember_zone_message(message_alias, payload, None, None, None);
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

#[when(expr = "I save current checkpoint of sequencer {string} as {string}")]
fn step_save_zone_checkpoint(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    checkpoint_alias: String,
) -> StepResult {
    save_zone_checkpoint(world, step, sequencer_alias, checkpoint_alias)
}

#[when(expr = "I restart zone sequencer {string} from checkpoint {string}")]
async fn step_restart_zone_sequencer_from_checkpoint(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    checkpoint_alias: String,
) -> StepResult {
    let checkpoint = world.zone.resolve_checkpoint(checkpoint_alias)?;
    let startup = world.zone.sequencer_startup_for(&sequencer_alias);

    start_named_sequencer_with_startup(world, step, &sequencer_alias, Some(checkpoint), startup)
        .await
}

#[when(expr = "I restart zone sequencer {string} fresh")]
async fn step_restart_zone_sequencer_fresh(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let startup = world.zone.sequencer_startup_for(&sequencer_alias);
    start_named_sequencer_with_startup(world, step, &sequencer_alias, None, startup).await
}

#[when(expr = "sequencer {string} submits zone config transaction {string} authorizing:")]
async fn step_submit_zone_channel_config_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        0,
        0,
    )
    .await
}

#[when(
    expr = "sequencer {string} submits zone config transaction {string} with posting timeframe {int} and timeout {int} authorizing:"
)]
async fn step_submit_zone_channel_config_transaction_with_posting_window(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    posting_timeframe: u32,
    posting_timeout: u32,
) -> StepResult {
    let authorized_aliases =
        single_column_table(step, "alias", "authorized zone sequencer aliases")?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        authorized_aliases,
        posting_timeframe,
        posting_timeout,
    )
    .await
}

#[when(expr = "sequencer {string} submits zone config transaction:")]
async fn step_submit_zone_channel_config_transaction_from_table(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    let row = zone_config_row(step)?;

    submit_zone_channel_config(
        world,
        step,
        &sequencer_alias,
        row.config_name,
        row.authorized_sequencers,
        row.posting_timeframe,
        row.posting_timeout,
    )
    .await
}

#[when(
    expr = "I submit zone deposit transaction {string} into channel of {string} of {int} with metadata {string}"
)]
async fn step_submit_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    channel_alias: String,
    amount: u64,
    metadata: String,
) -> StepResult {
    submit_zone_deposit_transaction(
        world,
        step,
        transaction_alias,
        channel_alias,
        amount,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(
    expr = "sequencer {string} submits atomic zone deposit transaction {string} with inscription {string} of {int} with metadata {string}"
)]
async fn step_submit_atomic_zone_deposit_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
    metadata: String,
) -> StepResult {
    submit_atomic_zone_deposit_transaction(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        message_alias,
        amount,
        metadata
            .into_bytes()
            .try_into()
            .expect("Metadata too large for deposit op."),
    )
    .await
}

#[when(
    expr = "sequencer {string} submits zone withdraw transaction {string} with inscription {string} of {int}"
)]
async fn step_submit_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    transaction_alias: String,
    message_alias: String,
    amount: u64,
) -> StepResult {
    submit_zone_withdraw_transaction(
        world,
        step,
        &sequencer_alias,
        transaction_alias,
        message_alias,
        amount,
    )
    .await
}

#[when(expr = "sequencer {string} publishes atomic withdraw {string} with inscription {string}:")]
async fn step_publish_atomic_zone_withdraw_transaction(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    bundle_alias: String,
    message_alias: String,
) -> StepResult {
    let withdraw_rows = zone_atomic_withdraw_rows(step)?;
    publish_atomic_zone_withdraw_transaction(
        world,
        step,
        &sequencer_alias,
        bundle_alias,
        message_alias,
        withdraw_rows,
    )
    .await
}

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
    let payloads = log_step_error(step, world.zone.message_payloads_for_aliases(&aliases))?;
    let mut view_rx = log_step_error(step, world.zone.sequencer_channel_view_rx(&sequencer_alias))?;
    wait_for_channel_view(&mut view_rx, Duration::from_secs(timeout_seconds), |view| {
        view.our_turn_to_write
    })
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    let (published, mempool_pending) = {
        let events = log_step_error(step, world.zone.sequencer_events_mut(&sequencer_alias))?;
        wait_for_adopted_payloads_and_collect_mempool_pending(
            events,
            &payloads,
            Duration::from_secs(timeout_seconds),
        )
        .await
        .map_err(|error| zone_step_error(step, &error))?
    };

    for ((message_alias, payload), published) in aliases.into_iter().zip(payloads).zip(published) {
        remember_published_zone_message(
            world,
            &sequencer_alias,
            message_alias,
            payload,
            &published,
        );
    }
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
    let payload = message_payload(world, &message_alias)?;
    let (published, mempool_pending) = {
        let events = log_step_error(step, world.zone.sequencer_events_mut(&sequencer_alias))?;
        wait_for_adopted_payload_and_collect_mempool_pending(
            events,
            &payload,
            Duration::from_secs(timeout_seconds),
        )
        .await
        .map_err(|error| zone_step_error(step, &error))?
    };

    remember_published_zone_message(world, &sequencer_alias, message_alias, payload, &published);
    world
        .zone
        .record_mempool_pending(sequencer_alias.clone(), mempool_pending);

    Ok(())
}

#[cucumber::then(expr = "all zone messages are safe in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_all_zone_messages_are_safe(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let inscription_ids = log_step_error(step, world.zone.ordered_inscription_ids())?;

    if !world.zone.has_published_messages() {
        return Err(StepError::LogicalError {
            message: "No zone messages have been published".to_owned(),
        });
    }

    let node = log_step_error(step, world.zone_node_http_client())?;

    ensure_zone_transactions_included(
        &node,
        &inscription_ids,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "all zone messages are finalized in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_all_zone_messages_are_finalized(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let inscription_ids = log_step_error(step, world.zone.ordered_inscription_ids())?;

    if !world.zone.has_published_messages() {
        return Err(StepError::LogicalError {
            message: "No zone messages have been published".to_owned(),
        });
    }

    let node_url = log_step_error(step, world.zone_node_url())?;

    wait_for_transactions_finalized(
        node_url,
        &inscription_ids,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(
    expr = "sequencer {string} emits the full transaction lifecycle for zone messages in {int} seconds:"
)]
#[cucumber::when(
    expr = "sequencer {string} emits the full transaction lifecycle for zone messages in {int} seconds:"
)]
async fn step_sequencer_emits_full_transaction_lifecycle(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let tx_hashes = log_step_error(step, world.zone.message_tx_hashes_for_aliases(&aliases))?;
    let mut tx_status_rx = log_step_error(
        step,
        world.zone.take_sequencer_tx_status_rx(&sequencer_alias),
    )?;

    wait_for_tx_status_lifecycle(
        &mut tx_status_rx,
        &tx_hashes,
        &[
            TxStatus::AcceptedLocally,
            TxStatus::PendingMempool,
            TxStatus::OnChain(TxSource::Local),
            TxStatus::Finalized(TxSource::Local),
        ],
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then("the zone indexer returns messages in this order:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_messages_in_order(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let expected = log_step_error(step, world.zone.message_payloads_for_aliases(&aliases))?;
    let indexer = log_step_error(step, world.zone.indexer())?;

    let actual = collect_indexed_messages(indexer, &expected, Duration::from_mins(3))
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    if actual == expected {
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: format!(
            "Zone indexer returned messages in unexpected order: expected {} messages, got {}",
            expected.len(),
            actual.len()
        ),
    })
}

#[cucumber::then(expr = "the zone indexer returns messages in any order in {int} seconds:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_messages_in_any_order(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let expected = log_step_error(step, world.zone.message_payloads_for_aliases(&aliases))?;
    let expected = expected.into_iter().collect::<HashSet<_>>();
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_indexer_unordered(indexer, &expected, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    Ok(())
}

#[cucumber::then("the zone indexer returns each of these messages exactly once in this order:")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_messages_exactly_once_in_order(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let expected = log_step_error(step, world.zone.message_payloads_for_aliases(&aliases))?;
    let indexer = log_step_error(step, world.zone.indexer())?;

    let actual = collect_indexed_messages_exactly_once(indexer, &expected, Duration::from_mins(3))
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    if actual == expected {
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: format!(
            "Zone indexer returned duplicate or out-of-order messages: expected {} messages, got {}",
            expected.len(),
            actual.len()
        ),
    })
}

#[cucumber::then(expr = "zone transaction {string} is included in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_transaction_is_included(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;
    let node = log_step_error(step, world.zone_node_http_client())?;

    ensure_zone_transactions_included(&node, &[tx_hash], Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "zone transaction {string} is finalized in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_transaction_is_finalized(
    world: &mut CucumberWorld,
    step: &Step,
    transaction_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let tx_hash = world.resolve_submitted_transaction(&transaction_alias)?;
    let node_url = log_step_error(step, world.zone_node_url())?;

    wait_for_transactions_finalized(node_url, &[tx_hash], Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "the zone indexer returns finalized deposit {string} in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_finalized_deposit(
    world: &mut CucumberWorld,
    step: &Step,
    deposit_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let (deposit, amount) = world
        .zone
        .resolve_submitted_deposit(&deposit_alias)?
        .clone();
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_deposit(
        indexer,
        &deposit,
        amount,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "the zone indexer returns finalized withdraw {string} in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_finalized_withdraw(
    world: &mut CucumberWorld,
    step: &Step,
    withdraw_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let withdraw = world
        .zone
        .resolve_submitted_withdraw(&withdraw_alias)?
        .clone();
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_withdraw(indexer, &withdraw, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "sequencer {string} finalizes deposit {string} in {int} seconds")]
async fn step_zone_sequencer_finalizes_deposit(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    deposit_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let (deposit, amount) = world
        .zone
        .resolve_submitted_deposit(&deposit_alias)?
        .clone();
    let events = log_step_error(step, world.zone.sequencer_events_mut(&sequencer_alias))?;

    let mempool_pending = wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending(
        events,
        &deposit,
        amount,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;
    world
        .zone
        .record_mempool_pending(sequencer_alias.clone(), mempool_pending);
    Ok(())
}

#[cucumber::then(expr = "sequencer {string} finalizes withdraw {string} in {int} seconds")]
async fn step_zone_sequencer_finalizes_withdraw(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    withdraw_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    let withdraw = world
        .zone
        .resolve_submitted_withdraw(&withdraw_alias)?
        .clone();
    let events = log_step_error(step, world.zone.sequencer_events_mut(&sequencer_alias))?;

    let mempool_pending = wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending(
        events,
        &withdraw,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;
    world
        .zone
        .record_mempool_pending(sequencer_alias.clone(), mempool_pending);
    Ok(())
}

#[cucumber::then(
    expr = "the zone indexer returns all zone messages exactly once in any order in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_all_messages_exactly_once_any_order(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let expected_set = published_payload_set(world, step)?;
    let indexer = log_step_error(step, world.zone.indexer())?;

    let seen =
        wait_for_indexer_unordered(indexer, &expected_set, Duration::from_secs(timeout_seconds))
            .await
            .map_err(|error| zone_step_error(step, &error))?;

    tokio::time::sleep(Duration::from_secs(CONCURRENT_DUPLICATE_SETTLE_SECS)).await;

    let all_payloads = scan_indexer_for_payloads(indexer, &expected_set)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    ensure_indexed_payloads_match_once(&expected_set, &seen, &all_payloads)
}

fn published_payload_set(
    world: &CucumberWorld,
    step: &Step,
) -> Result<HashSet<Inscription>, StepError> {
    let expected_payloads = log_step_error(step, world.zone.published_message_payloads())?;

    Ok(expected_payloads.into_iter().collect())
}

fn message_payload(world: &CucumberWorld, message_alias: &str) -> Result<Inscription, StepError> {
    let payloads = world
        .zone
        .message_payloads_for_aliases(&[message_alias.to_owned()])?;

    payloads.into_iter().next().ok_or(StepError::LogicalError {
        message: format!("Zone message alias '{message_alias}' not found"),
    })
}

fn ensure_indexed_payloads_match_once(
    expected: &HashSet<Inscription>,
    seen: &HashSet<Inscription>,
    all_payloads: &[Inscription],
) -> StepResult {
    let unique: HashSet<&Inscription> = all_payloads.iter().collect();

    if unique.len() != all_payloads.len() {
        return Err(StepError::LogicalError {
            message: format!(
                "Duplicate inscriptions detected on chain: expected {} unique, got {} total",
                unique.len(),
                all_payloads.len()
            ),
        });
    }

    if unique.len() != expected.len() || seen != expected {
        return Err(StepError::LogicalError {
            message: format!(
                "Zone indexer did not return the expected message set: expected {}, got {}",
                expected.len(),
                unique.len()
            ),
        });
    }

    Ok(())
}

#[cucumber::then(
    expr = "the zone indexer returns {int} copies of zone message {string} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_payload_count(
    world: &mut CucumberWorld,
    step: &Step,
    expected_count: usize,
    payload: String,
    timeout_seconds: u64,
) -> StepResult {
    let indexer = log_step_error(step, world.zone.indexer())?;
    wait_for_exact_indexed_payload_count(
        indexer,
        make_inscription(&payload),
        expected_count,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(expr = "zone balance updates keep all accounts non-negative after {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_balance_updates_keep_accounts_non_negative(
    world: &mut CucumberWorld,
    step: &Step,
    settle_seconds: u64,
) -> StepResult {
    tokio::time::sleep(Duration::from_secs(settle_seconds)).await;

    let mut balances = world.zone.zone_account_balances()?;
    let expected_set = published_payload_set(world, step)?;
    let indexer = log_step_error(step, world.zone.indexer())?;
    let on_chain = scan_indexer_for_payloads(indexer, &expected_set)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    apply_indexed_balance_updates(&mut balances, &on_chain);

    ensure_balances_non_negative(&balances)
}

#[cucumber::then(
    expr = "the zone indexer preserves per-sequencer order and converges without duplicates in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_preserves_per_sequencer_order_without_duplicates(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let expected_set = published_payload_set(world, step)?;
    let total = world.zone.sorted_total_payloads()?;
    let expected_by_sequencer = world.zone.sorted_expected_by_sequencer()?;
    let discarded = log_step_error(step, world.zone.discarded_payloads(DEFAULT_ZONE_SEQUENCER))?;
    let indexer = log_step_error(step, world.zone.indexer())?;

    let on_chain = wait_until_sorted_conflict_settles(
        indexer,
        &expected_set,
        &discarded,
        total,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;

    let discarded_snapshot = discarded.lock().await.clone();
    assert_sorted_outcome(
        &on_chain,
        &discarded_snapshot,
        total,
        &expected_by_sequencer,
    )
}

fn apply_indexed_balance_updates(balances: &mut HashMap<String, i64>, payloads: &[Inscription]) {
    for payload in payloads {
        let Some((_, account, delta)) = parse_balance_payload(payload) else {
            continue;
        };

        *balances.entry(account).or_default() += delta;
    }
}

fn ensure_balances_non_negative(balances: &HashMap<String, i64>) -> StepResult {
    let negative = balances
        .iter()
        .filter(|(_, balance)| **balance < 0)
        .map(|(account, balance)| format!("{account}={balance}"))
        .collect::<Vec<_>>();

    if negative.is_empty() {
        return Ok(());
    }

    Err(StepError::LogicalError {
        message: format!(
            "Zone account balances went negative: {}",
            negative.join(", ")
        ),
    })
}
