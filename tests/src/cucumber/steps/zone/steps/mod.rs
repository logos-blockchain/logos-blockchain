use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use cucumber::{gherkin::Step, given, when};
use lb_core::mantle::ops::channel::inscribe::Inscription;

use super::{
    CustomRepublishDeps, PublishDeadline,
    actions::{
        DriveMode, initialize_zone_indexer, prepare_zone_channel_config,
        publish_atomic_zone_withdraw_transaction, publish_zone_messages,
        publish_zone_messages_concurrently, register_zone_sequencers_with_shared_key,
        remember_published_zone_message, save_zone_checkpoint, sign_prepared_zone_channel_config,
        start_deposit_reaction_sequencer, start_deposit_withdraw_sequencer, start_named_sequencer,
        start_named_sequencer_with_pending_submit_depth, start_nodes_with_zone_resources,
        stop_zone_sequencer, submit_atomic_zone_deposit_transaction,
        submit_prepared_zone_channel_config, submit_zone_channel_config,
        submit_zone_channel_split_transaction, submit_zone_deposit_transaction,
        submit_zone_multi_deposit_transaction, submit_zone_withdraw_transaction,
    },
    assertions::{
        assert_sorted_outcome, scan_indexer_for_payloads, wait_for_indexer_unordered,
        wait_until_sorted_conflict_settles,
    },
    balance_update_payload, collect_indexed_messages, collect_indexed_messages_exactly_once,
    ensure_zone_transactions_included,
    errors::{log_step_error, zone_step_error},
    parse_balance_payload, publish_message_with_retry,
    runner::{TxSource, TxStatus},
    tables::{
        ConcurrentZoneMessageRow, GeneratedZoneMessageBatch, concurrent_zone_message_rows,
        custom_tx_rows, generated_zone_message_batches, generated_zone_message_sequencers,
        group_zone_messages_by_sequencer, zone_account_balances, zone_atomic_withdraw_rows,
        zone_balance_rows, zone_config_row, zone_message_rows, zone_node_resource_rows,
        zone_sequencer_start_rows, zone_sequencing_state_row,
    },
    wait_for_channel_transfer_input_count, wait_for_channel_view, wait_for_channel_wallet_counts,
    wait_for_channel_wallet_note, wait_for_deposit, wait_for_exact_indexed_payload_count,
    wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending,
    wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending, wait_for_lib_advance,
    wait_for_on_chain_statuses_and_collect_mempool_pending, wait_for_transactions_finalized,
    wait_for_turn_to_write, wait_for_tx_status_lifecycle, wait_for_withdraw,
};
use crate::{
    common::mantle_inscription::make_inscription,
    cucumber::{
        error::{StepError, StepResult},
        steps::parse_steps::single_column_table,
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

mod assertions;
mod channel;
mod lifecycle;
mod policies;
mod publishing;
mod sequencing;
