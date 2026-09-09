use super::{
    CONCURRENT_DUPLICATE_SETTLE_SECS, CucumberWorld, DEFAULT_ZONE_SEQUENCER, Duration, HashMap,
    HashSet, Inscription, Step, StepError, StepResult, TxSource, TxStatus, assert_sorted_outcome,
    collect_indexed_messages, collect_indexed_messages_exactly_once,
    ensure_zone_transactions_included, log_step_error, make_inscription, parse_balance_payload,
    scan_indexer_for_payloads, single_column_table, wait_for_channel_transfer_input_count,
    wait_for_channel_wallet_counts, wait_for_channel_wallet_note, wait_for_deposit,
    wait_for_exact_indexed_payload_count,
    wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending,
    wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending,
    wait_for_indexer_unordered, wait_for_transactions_finalized, wait_for_tx_status_lifecycle,
    wait_for_withdraw, wait_until_sorted_conflict_settles, zone_step_error,
};

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

#[cucumber::then(
    expr = "the zone indexer returns a finalized channel transfer consuming {int} inputs in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_returns_finalized_channel_transfer_input_count(
    world: &mut CucumberWorld,
    step: &Step,
    expected_inputs: usize,
    timeout_seconds: u64,
) -> StepResult {
    let indexer = log_step_error(step, world.zone.indexer())?;

    wait_for_channel_transfer_input_count(
        indexer,
        expected_inputs,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(
    expr = "the channel wallet of {string} contains a note of value {int} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_channel_wallet_contains_note(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    value: u64,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone.sequencer_client(&sequencer_alias))?;
    wait_for_channel_wallet_note(client, value, false, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(
    expr = "the channel wallet of {string} contains a finalized note of value {int} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_channel_wallet_contains_finalized_note(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    value: u64,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone.sequencer_client(&sequencer_alias))?;
    wait_for_channel_wallet_note(client, value, true, Duration::from_secs(timeout_seconds))
        .await
        .map_err(|error| zone_step_error(step, &error))
}

#[cucumber::then(
    expr = "the channel wallet of {string} has exactly {int} finalized and {int} unfinalized notes in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_channel_wallet_has_exact_counts(
    world: &mut CucumberWorld,
    step: &Step,
    sequencer_alias: String,
    finalized: usize,
    unfinalized: usize,
    timeout_seconds: u64,
) -> StepResult {
    let client = log_step_error(step, world.zone.sequencer_client(&sequencer_alias))?;
    wait_for_channel_wallet_counts(
        client,
        finalized,
        unfinalized,
        Duration::from_secs(timeout_seconds),
    )
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
