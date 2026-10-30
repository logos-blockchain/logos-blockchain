use lb_core::mantle::{
    ledger::NoteId,
    ops::{OpId as _, OpRef},
};

use super::{
    CONCURRENT_DUPLICATE_SETTLE_SECS, CucumberWorld, DEFAULT_ZONE_SEQUENCER, Duration, HashMap,
    HashSet, Inscription, Step, StepError, StepResult, assert_sorted_outcome,
    collect_indexed_messages, collect_indexed_messages_exactly_once, ensure_zone_message_included,
    ensure_zone_transactions_included, log_step_error, make_inscription, parse_balance_payload,
    pin_payload, replay_finalized_history, scan_indexer_for_payloads, single_column_table,
    wait_for_channel_transfer_input_count, wait_for_channel_wallet_counts,
    wait_for_channel_wallet_note, wait_for_deposit, wait_for_exact_indexed_payload_count,
    wait_for_finalized_deposit_via_sequencer, wait_for_finalized_withdraw_via_sequencer,
    wait_for_indexer_unordered, wait_for_transactions_finalized, wait_for_withdraw,
    wait_until_sorted_conflict_settles, withdraw_payload, zone_step_error,
};

#[cucumber::then("the channel view contract holds for all zone sequencers")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
fn step_channel_view_contract_holds(world: &mut CucumberWorld) -> StepResult {
    let violations = world.zone.view_violations();
    if violations.is_empty() {
        return Ok(());
    }
    Err(StepError::LogicalError {
        message: violations
            .iter()
            .map(|(alias, message)| format!("{alias}: {message}"))
            .collect::<Vec<_>>()
            .join("; "),
    })
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

/// The message is mined on the named node's tip chain, under whatever tx
/// hash carries it. Use it where a scenario needs the inscription in a block
/// before the next step acts, instead of a height wait that may already be
/// satisfied.
#[cucumber::when(
    expr = "zone message {string} from sequencer {string} is included on node {string} in {int} seconds"
)]
#[cucumber::then(
    expr = "zone message {string} from sequencer {string} is included on node {string} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_message_is_included_on_node(
    world: &mut CucumberWorld,
    step: &Step,
    message_alias: String,
    sequencer_alias: String,
    node_name: String,
    timeout_seconds: u64,
) -> StepResult {
    let msg_id = log_step_error(step, world.zone.published_message(&message_alias))?
        .msg_id
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Zone message '{message_alias}' has no tracked message id"),
        })?;
    let channel_id = world.zone.sequencer_channel_id(&sequencer_alias)?;
    let node = world.resolve_node_http_client(&node_name)?;

    ensure_zone_message_included(
        &node,
        channel_id,
        msg_id,
        Duration::from_secs(timeout_seconds),
    )
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
#[cucumber::when(
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

    wait_for_finalized_deposit_via_sequencer(
        events,
        &deposit,
        amount,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;
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

    wait_for_finalized_withdraw_via_sequencer(
        events,
        &withdraw,
        Duration::from_secs(timeout_seconds),
    )
    .await
    .map_err(|error| zone_step_error(step, &error))?;
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

/// The message finalized, but not as the tx the publish returned: the SDK
/// re-funded it under a new hash after the original was stuck.
#[cucumber::then(expr = "zone message {string} finalized under a different tx hash than submitted")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_message_finalized_under_a_rebuilt_tx(
    world: &mut CucumberWorld,
    step: &Step,
    message_alias: String,
) -> StepResult {
    let message = log_step_error(step, world.zone.published_message(&message_alias))?;
    let submitted = message
        .inscription_id
        .ok_or_else(|| StepError::LogicalError {
            message: format!("Zone message '{message_alias}' has no submitted tx hash"),
        })?;
    let payload = message.payload.clone();
    let reader = log_step_error(step, world.zone.indexer())?;
    let history = replay_finalized_history(reader)
        .await
        .map_err(|error| zone_step_error(step, &error))?;

    let finalized_as: Vec<_> = history
        .iter()
        .filter(|tx| {
            tx.ops.iter().any(|op| {
                matches!(op, lb_zone_sdk::sequencer::FinalizedOp::Inscription(info) if info.payload == payload)
            })
        })
        .map(|tx| tx.tx_hash)
        .collect();
    if finalized_as.is_empty() {
        return Err(StepError::LogicalError {
            message: format!("Zone message '{message_alias}' is not finalized"),
        });
    }
    if finalized_as.contains(&submitted) {
        return Err(StepError::LogicalError {
            message: format!(
                "Zone message '{message_alias}' finalized as the tx it was submitted with; nothing was re-funded"
            ),
        });
    }
    Ok(())
}

/// Messages whose checkpointed txs spend a common fee note: at most one of
/// them finalizes as submitted, every other one under a rebuilt tx hash.
/// Messages sharing no note have nothing to re-fund and pass.
#[cucumber::then(
    expr = "zone messages double-handed in checkpoint {string} finalized with the losers re-funded:"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_double_handed_messages_are_refunded(
    world: &mut CucumberWorld,
    step: &Step,
    checkpoint_alias: String,
) -> StepResult {
    let aliases = single_column_table(step, "alias", "zone message aliases")?;
    let checkpoint = log_step_error(step, world.zone.resolve_checkpoint(&checkpoint_alias))?;
    let mut messages = Vec::new();
    for alias in aliases {
        let message = log_step_error(step, world.zone.published_message(&alias))?;
        let submitted = message
            .inscription_id
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone message '{alias}' has no submitted tx hash"),
            })?;
        let fee_notes: HashSet<NoteId> = checkpoint
            .pending_txs
            .iter()
            .filter(|(hash, _)| *hash == submitted)
            .flat_map(|(_, tx)| tx.op_refs())
            .filter_map(|op| match op {
                OpRef::Transfer(transfer) => Some(transfer.inputs.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect();
        messages.push((alias, submitted, message.payload.clone(), fee_notes));
    }

    let reader = log_step_error(step, world.zone.indexer())?;
    let history = replay_finalized_history(reader)
        .await
        .map_err(|error| zone_step_error(step, &error))?;
    let finalized_as = |payload: &Inscription| {
        history
            .iter()
            .filter(|tx| {
                tx.ops.iter().any(|op| {
                    matches!(op, lb_zone_sdk::sequencer::FinalizedOp::Inscription(info) if info.payload == *payload)
                })
            })
            .map(|tx| tx.tx_hash)
            .collect::<Vec<_>>()
    };

    for (index, (alias, submitted, payload, fee_notes)) in messages.iter().enumerate() {
        let rivals: Vec<&str> = messages
            .iter()
            .enumerate()
            .filter(|(other, (_, _, _, notes))| *other != index && !notes.is_disjoint(fee_notes))
            .map(|(_, (alias, ..))| alias.as_str())
            .collect();
        if rivals.is_empty() {
            continue;
        }
        let finalized = finalized_as(payload);
        if finalized.is_empty() {
            return Err(StepError::LogicalError {
                message: format!(
                    "Zone message '{alias}' shares a fee note with {rivals:?} and is not finalized"
                ),
            });
        }
        if finalized.contains(submitted) {
            let other_winner = messages
                .iter()
                .filter(|(other, other_submitted, other_payload, _)| {
                    rivals.contains(&other.as_str())
                        && finalized_as(other_payload).contains(other_submitted)
                })
                .map(|(other, ..)| other.as_str())
                .next();
            if let Some(other) = other_winner {
                return Err(StepError::LogicalError {
                    message: format!(
                        "Zone messages '{alias}' and '{other}' both finalized as submitted while spending the same fee note"
                    ),
                });
            }
        }
    }
    Ok(())
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
    expr = "the zone indexer returns exactly one finalized pin and withdraw for deposit {string} in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require `&mut World` as the first parameter"
)]
async fn step_zone_indexer_exact_reactive_lifecycle(
    world: &mut CucumberWorld,
    step: &Step,
    deposit_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    // Assert each reactive-policy payload for this op_id appears exactly once:
    // no missing phase, no surviving duplicate from a benign double-publish.
    let op_id = log_step_error(step, world.zone.resolve_submitted_deposit(&deposit_alias))?
        .0
        .op_id();
    let indexer = log_step_error(step, world.zone.indexer())?;
    let duration = Duration::from_secs(timeout_seconds);
    wait_for_exact_indexed_payload_count(indexer, pin_payload(&op_id), 1, duration)
        .await
        .map_err(|error| zone_step_error(step, &error))?;
    wait_for_exact_indexed_payload_count(indexer, withdraw_payload(&op_id), 1, duration)
        .await
        .map_err(|error| zone_step_error(step, &error))
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
