use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use cucumber::{gherkin::Step, given, when};

use super::{
    actions::{
        DriveMode, ensure_zone_sequencer_exists, initialize_zone_indexer,
        publish_balance_updates_concurrently, publish_zone_messages,
        publish_zone_messages_concurrently, register_zone_sequencers,
        register_zone_sequencers_with_shared_key, save_zone_checkpoint, start_named_sequencer,
        start_zone_cluster, stop_zone_sequencer, submit_atomic_zone_deposit_transaction,
        submit_zone_channel_config, submit_zone_deposit_transaction,
        submit_zone_withdraw_transaction,
    },
    assertions::{
        assert_sorted_outcome, scan_indexer_for_payloads, wait_for_indexer_unordered,
        wait_until_sorted_conflict_settles,
    },
    errors::{log_step_error, zone_step_error},
    support::{
        collect_indexed_messages, collect_indexed_messages_exactly_once,
        ensure_zone_transactions_included, parse_balance_payload, wait_for_deposit,
        wait_for_exact_indexed_payload_count, wait_for_lib_advance,
        wait_for_transactions_finalized, wait_for_withdraw,
    },
    tables::{
        ConcurrentZoneMessageRow, GeneratedZoneMessageBatch, concurrent_zone_message_rows,
        generated_zone_message_batches, generated_zone_message_sequencers,
        group_zone_messages_by_sequencer, single_column_table, zone_account_balances,
        zone_balance_rows, zone_message_rows,
    },
};
use crate::{
    common::manual_cluster::wait_for_height,
    cucumber::{
        error::{StepError, StepResult},
        world::CucumberWorld,
    },
};

pub(super) const DEFAULT_ZONE_SEQUENCER: &str = "SEQ_A";
const CONCURRENT_DUPLICATE_SETTLE_SECS: u64 = 30;

#[given("I have a zone cluster")]
async fn step_zone_cluster(world: &mut CucumberWorld, step: &Step) -> StepResult {
    start_zone_cluster(world, step).await
}

#[given("the following zone sequencers exist:")]
fn step_zone_sequencers_exist(world: &mut CucumberWorld, step: &Step) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone sequencer aliases")?;
    register_zone_sequencers(world, aliases);

    Ok(())
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

#[given("a zone sequencer is initialized")]
#[when("a zone sequencer is initialized")]
async fn step_default_zone_sequencer_initialized(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    ensure_zone_sequencer_exists(world, DEFAULT_ZONE_SEQUENCER);
    start_sequencer_with_indexer(world, step, DEFAULT_ZONE_SEQUENCER).await
}

#[when(expr = "I start zone sequencer {string}")]
async fn step_start_zone_sequencer(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::Passive).await
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
    start_named_sequencer(world, step, sequencer_alias, None, DriveMode::Passive).await?;
    initialize_zone_indexer(world, step, sequencer_alias)
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

#[cucumber::when(expr = "the zone node is at height {int} in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_node_is_at_height(
    world: &mut CucumberWorld,
    step: &Step,
    height: u64,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone_node_http_client())?;

    wait_for_height(&client, height, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|_| StepError::Timeout {
            message: format!(
                "Zone node did not reach height {height} in {timeout_seconds} seconds"
            ),
        })
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

    start_named_sequencer(
        world,
        step,
        &sequencer_alias,
        Some(checkpoint),
        DriveMode::Passive,
    )
    .await
}

#[when(expr = "I restart zone sequencer {string} fresh")]
async fn step_restart_zone_sequencer_fresh(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
) -> StepResult {
    start_named_sequencer(world, step, &sequencer_alias, None, DriveMode::Passive).await
}

#[when(expr = "sequencer {string} submits zone channel config transaction {string} authorizing:")]
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
    expr = "sequencer {string} submits zone channel config transaction {string} with posting timeframe {int} and posting timeout {int} authorizing:"
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
        metadata,
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
        metadata,
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
        payload,
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
    payload: String,
) -> Vec<ConcurrentZoneMessageRow> {
    let mut builder = GeneratedZoneMessages::default();
    let payload = payload.into_bytes();

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
                format!("{data_prefix}{payload_number}").into_bytes(),
            );
        }
    }

    fn append_repeated_payloads(&mut self, sequencer_alias: String, count: usize, payload: &[u8]) {
        for _ in 1..count {
            self.push(sequencer_alias.clone(), payload.to_vec());
        }

        if count > 0 {
            self.push(sequencer_alias, payload.to_vec());
        }
    }

    fn push(&mut self, sequencer_alias: String, payload: Vec<u8>) {
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

    for sequencer_alias in grouped.keys() {
        start_named_sequencer(world, step, sequencer_alias, None, DriveMode::Republish).await?;
    }

    publish_zone_messages_concurrently(world, step, rows).await
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

    publish_zone_messages_concurrently(world, step, rows).await
}

#[when("the following zone balance updates are published concurrently with balance-aware policy:")]
async fn step_publish_zone_balance_updates_with_balance_policy(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    let rows = zone_balance_rows(step)?;
    let initial_balances = world.zone.zone_account_balances()?;
    let sequencers = rows
        .iter()
        .map(|row| row.sequencer_alias.clone())
        .collect::<HashSet<_>>();

    for sequencer_alias in sequencers {
        start_named_sequencer(
            world,
            step,
            sequencer_alias,
            None,
            DriveMode::BalanceAware {
                initial_balances: initial_balances.clone(),
            },
        )
        .await?;
    }

    publish_balance_updates_concurrently(world, step, rows).await
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
    let deposit = world
        .zone
        .resolve_submitted_deposit(&deposit_alias)?
        .clone();
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_deposit(indexer, &deposit, Duration::from_secs(timeout_seconds))
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
) -> Result<HashSet<Vec<u8>>, StepError> {
    let expected_payloads = log_step_error(step, world.zone.published_message_payloads())?;

    Ok(expected_payloads.into_iter().collect())
}

fn ensure_indexed_payloads_match_once(
    expected: &HashSet<Vec<u8>>,
    seen: &HashSet<Vec<u8>>,
    all_payloads: &[Vec<u8>],
) -> StepResult {
    let unique: HashSet<&Vec<u8>> = all_payloads.iter().collect();

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
        payload.as_bytes(),
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

#[cucumber::then(expr = "the zone indexer returns a sorted zone conflict outcome in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_sorted_zone_conflict_outcome(
    world: &mut CucumberWorld,
    step: &Step,
    timeout_seconds: u64,
) -> StepResult {
    let expected_set = published_payload_set(world, step)?;
    let total = world.zone.sorted_total_payloads()?;
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
    assert_sorted_outcome(&on_chain, &discarded_snapshot, total)
}

fn apply_indexed_balance_updates(balances: &mut HashMap<String, i64>, payloads: &[Vec<u8>]) {
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
