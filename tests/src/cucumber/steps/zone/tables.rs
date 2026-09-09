use std::collections::{HashMap, HashSet};

use cucumber::gherkin::Step;
use lb_core::mantle::ops::channel::inscribe::Inscription;

use crate::{
    common::mantle_inscription::make_inscription,
    cucumber::{
        error::StepError,
        steps::parse_steps::{
            comma_separated_aliases, invalid_table_row, optional_string_cell, parse_list_cell,
            parse_required_bool_cell, parse_required_string_cell, parse_single_table_row,
            parse_table_rows, parse_u32_cell, parse_u64_cell, parse_usize_cell,
        },
    },
};

#[derive(Clone)]
pub(super) struct ConcurrentZoneMessageRow {
    pub sequencer_alias: String,
    pub message_alias: String,
    pub payload: Inscription,
}

#[derive(Clone)]
pub(super) struct ZoneBalanceRow {
    pub sequencer_alias: String,
    pub message_alias: String,
    pub account: String,
    pub delta: i64,
}

pub(super) struct ZoneAccountBalance {
    pub account: String,
    pub balance: i64,
}

pub(super) struct GeneratedZoneMessageBatch {
    pub sequencer_alias: String,
    pub data_prefix: String,
}

pub(super) struct ZoneSequencerStartRow {
    pub alias: String,
    pub indexer: bool,
    pub pending_submit_depth: String,
    pub passive_republish_orphans: bool,
}

pub(super) struct ZoneSequencingStateRow {
    pub own_key_index: usize,
    pub is_our_turn: bool,
    pub pending_transactions: usize,
    pub timeout_seconds: u64,
}

pub(super) struct ZoneConfigRow {
    pub config_name: String,
    pub posting_timeframe: u32,
    pub posting_timeout: u32,
    pub authorized_sequencers: Vec<String>,
}

pub(super) struct ZoneNodeResourcesRow {
    pub node_name: String,
    pub account_index: usize,
    pub wallet_name: String,
    pub connected_to: Option<String>,
    pub sequencers: Vec<String>,
}

pub(super) fn zone_node_resource_rows(step: &Step) -> Result<Vec<ZoneNodeResourcesRow>, StepError> {
    let rows = parse_table_rows(
        step,
        &[
            "node_name",
            "account_index",
            "wallet_name",
            "connected_to",
            "sequencers",
        ],
        "Zone node",
        parse_zone_node_resource_row,
    )?;

    ensure_unique_zone_node_resources(&rows)?;

    Ok(rows)
}

fn parse_zone_node_resource_row(row: &[String]) -> Result<ZoneNodeResourcesRow, StepError> {
    match row {
        [
            node_name,
            account_index,
            wallet_name,
            connected_to,
            sequencers,
        ] => {
            let node_name = parse_required_string_cell(node_name, "node_name", "Zone node")?;
            let account_index =
                account_index
                    .trim()
                    .parse()
                    .map_err(|error| StepError::InvalidArgument {
                        message: format!("Invalid zone account index '{account_index}': {error}"),
                    })?;
            let wallet_name = parse_required_string_cell(wallet_name, "wallet_name", "Zone node")?;
            let connected_to = optional_string_cell(connected_to);
            let sequencers = comma_separated_aliases(sequencers, "sequencers", "Zone node")?;

            Ok(ZoneNodeResourcesRow {
                node_name,
                account_index,
                wallet_name,
                connected_to,
                sequencers,
            })
        }
        _ => invalid_table_row(
            "Zone node",
            &[
                "node_name",
                "account_index",
                "wallet_name",
                "connected_to",
                "sequencers",
            ],
            row.len(),
        ),
    }
}

fn ensure_unique_zone_node_resources(rows: &[ZoneNodeResourcesRow]) -> Result<(), StepError> {
    let mut wallets = HashSet::new();
    let mut sequencers = HashSet::new();

    for row in rows {
        if !wallets.insert(row.wallet_name.clone()) {
            return Err(StepError::InvalidArgument {
                message: format!("Duplicate zone wallet '{}'", row.wallet_name),
            });
        }

        for sequencer in &row.sequencers {
            if !sequencers.insert(sequencer.clone()) {
                return Err(StepError::InvalidArgument {
                    message: format!("Duplicate zone sequencer '{sequencer}'"),
                });
            }
        }
    }

    Ok(())
}

pub(super) fn zone_message_rows(step: &Step) -> Result<Vec<(String, Inscription)>, StepError> {
    parse_table_rows(step, &["alias", "data"], "Zone message", |row| match row {
        [alias, data] => Ok((alias.clone(), make_inscription(data))),
        _ => invalid_table_row("Zone message", &["alias", "data"], row.len()),
    })
}

pub(super) fn concurrent_zone_message_rows(
    step: &Step,
) -> Result<Vec<ConcurrentZoneMessageRow>, StepError> {
    parse_table_rows(
        step,
        &["sequencer", "alias", "data"],
        "Concurrent zone message",
        |row| match row {
            [sequencer_alias, message_alias, data] => Ok(ConcurrentZoneMessageRow {
                sequencer_alias: sequencer_alias.clone(),
                message_alias: message_alias.clone(),
                payload: make_inscription(data),
            }),
            _ => invalid_table_row(
                "Concurrent zone message",
                &["sequencer", "alias", "data"],
                row.len(),
            ),
        },
    )
}

#[derive(Clone)]
pub(super) struct CustomTxRow {
    pub sequencer_alias: String,
    pub transactions: u32,
    pub inscriptions: u32,
}

pub(super) fn custom_tx_rows(step: &Step) -> Result<Vec<CustomTxRow>, StepError> {
    parse_table_rows(
        step,
        &["sequencer", "transactions", "inscriptions"],
        "Concurrent custom transaction",
        |row| match row {
            [sequencer_alias, transactions, inscriptions] => Ok(CustomTxRow {
                sequencer_alias: sequencer_alias.clone(),
                transactions: parse_u32_cell(transactions, "transactions")?,
                inscriptions: parse_u32_cell(inscriptions, "inscriptions")?,
            }),
            _ => invalid_table_row(
                "Concurrent custom transaction",
                &["sequencer", "transactions", "inscriptions"],
                row.len(),
            ),
        },
    )
}

pub(super) fn group_zone_messages_by_sequencer(
    rows: &[ConcurrentZoneMessageRow],
) -> HashMap<String, Vec<ConcurrentZoneMessageRow>> {
    let mut grouped = HashMap::new();

    for row in rows {
        grouped
            .entry(row.sequencer_alias.clone())
            .or_insert_with(Vec::new)
            .push(row.clone());
    }

    grouped
}

pub(super) fn generated_zone_message_batches(
    step: &Step,
) -> Result<Vec<GeneratedZoneMessageBatch>, StepError> {
    parse_table_rows(
        step,
        &["sequencer", "data_prefix"],
        "Generated zone message batch",
        |row| match row {
            [sequencer_alias, data_prefix] => Ok(GeneratedZoneMessageBatch {
                sequencer_alias: sequencer_alias.clone(),
                data_prefix: data_prefix.clone(),
            }),
            _ => invalid_table_row(
                "Generated zone message batch",
                &["sequencer", "data_prefix"],
                row.len(),
            ),
        },
    )
}

pub(super) fn generated_zone_message_sequencers(step: &Step) -> Result<Vec<String>, StepError> {
    parse_table_rows(
        step,
        &["sequencer"],
        "Generated zone message sequencer",
        |row| match row {
            [sequencer_alias] => Ok(sequencer_alias.clone()),
            _ => invalid_table_row(
                "Generated zone message sequencer",
                &["sequencer"],
                row.len(),
            ),
        },
    )
}

pub(super) fn zone_sequencer_start_rows(
    step: &Step,
) -> Result<Vec<ZoneSequencerStartRow>, StepError> {
    parse_table_rows(
        step,
        &[
            "alias",
            "indexer",
            "pending_submit_depth",
            "passive_republish_orphans",
        ],
        "Zone sequencer startup",
        |row| match row {
            [
                alias,
                indexer,
                pending_submit_depth,
                passive_republish_orphans,
            ] => Ok(ZoneSequencerStartRow {
                alias: alias.clone(),
                indexer: parse_required_bool_cell(indexer, "indexer", "Zone sequencer startup")?,
                pending_submit_depth: pending_submit_depth.clone(),
                passive_republish_orphans: parse_required_bool_cell(
                    passive_republish_orphans,
                    "passive_republish_orphans",
                    "Zone sequencer startup",
                )?,
            }),
            _ => invalid_table_row(
                "Zone sequencer startup",
                &[
                    "alias",
                    "indexer",
                    "pending_submit_depth",
                    "passive_republish_orphans",
                ],
                row.len(),
            ),
        },
    )
}

pub(super) fn zone_sequencing_state_row(step: &Step) -> Result<ZoneSequencingStateRow, StepError> {
    parse_single_table_row(
        step,
        &[
            "own_key_index",
            "turn_to_write",
            "pending_transactions",
            "time_out",
        ],
        "Zone sequencing state",
        |row| match row {
            [
                own_key_index,
                turn_to_write,
                pending_transactions,
                timeout_seconds,
            ] => Ok(ZoneSequencingStateRow {
                own_key_index: parse_usize_cell(own_key_index, "own_key_index")?,
                is_our_turn: parse_turn_cell(turn_to_write)?,
                pending_transactions: parse_usize_cell(
                    pending_transactions,
                    "pending_transactions",
                )?,
                timeout_seconds: parse_u64_cell(timeout_seconds, "time_out")?,
            }),
            _ => invalid_table_row(
                "Zone sequencing state",
                &[
                    "own_key_index",
                    "turn_to_write",
                    "pending_transactions",
                    "time_out",
                ],
                row.len(),
            ),
        },
    )
}

pub(super) fn zone_config_row(step: &Step) -> Result<ZoneConfigRow, StepError> {
    parse_single_table_row(
        step,
        &[
            "config_name",
            "posting_timeframe",
            "posting_timeout",
            "authorized_sequencers",
        ],
        "Zone config transaction",
        |row| match row {
            [
                config_name,
                posting_timeframe,
                posting_timeout,
                authorized_sequencers,
            ] => Ok(ZoneConfigRow {
                config_name: config_name.clone(),
                posting_timeframe: parse_u32_cell(posting_timeframe, "posting_timeframe")?,
                posting_timeout: parse_u32_cell(posting_timeout, "posting_timeout")?,
                authorized_sequencers: parse_list_cell(
                    authorized_sequencers,
                    "authorized_sequencers",
                    "Zone config transaction",
                )?,
            }),
            _ => invalid_table_row(
                "Zone config transaction",
                &[
                    "config_name",
                    "posting_timeframe",
                    "posting_timeout",
                    "authorized_sequencers",
                ],
                row.len(),
            ),
        },
    )
}

pub(super) fn zone_account_balances(step: &Step) -> Result<Vec<ZoneAccountBalance>, StepError> {
    parse_table_rows(
        step,
        &["account", "balance"],
        "Zone account balance",
        |row| match row {
            [account, balance] => Ok(ZoneAccountBalance {
                account: account.clone(),
                balance: balance
                    .parse()
                    .map_err(|error| StepError::InvalidArgument {
                        message: format!("Invalid zone account balance '{balance}': {error}"),
                    })?,
            }),
            _ => invalid_table_row("Zone account balance", &["account", "balance"], row.len()),
        },
    )
}

pub(super) fn zone_balance_rows(step: &Step) -> Result<Vec<ZoneBalanceRow>, StepError> {
    parse_table_rows(
        step,
        &["sequencer", "alias", "account", "delta"],
        "Zone balance update",
        |row| match row {
            [sequencer_alias, message_alias, account, delta] => Ok(ZoneBalanceRow {
                sequencer_alias: sequencer_alias.clone(),
                message_alias: message_alias.clone(),
                account: account.clone(),
                delta: delta.parse().map_err(|error| StepError::InvalidArgument {
                    message: format!("Invalid zone balance delta '{delta}': {error}"),
                })?,
            }),
            _ => invalid_table_row(
                "Zone balance update",
                &["sequencer", "alias", "account", "delta"],
                row.len(),
            ),
        },
    )
}

/// Parses an atomic-withdraw table row of `(alias, outputs)` where `outputs`
/// is a comma-separated list of note values. Each row becomes one
/// `WithdrawArg`; the comma list lets a single arg carry multiple output
/// notes so the SDK's `publish_atomic_withdraw(inscribe, vec![WithdrawArg])`
/// signature is exercised at full width (multi-arg + multi-output-per-arg).
pub(super) fn zone_atomic_withdraw_rows(step: &Step) -> Result<Vec<(String, Vec<u64>)>, StepError> {
    parse_table_rows(
        step,
        &["withdraw", "outputs"],
        "Atomic withdraw",
        |row| match row {
            [alias, outputs] => {
                let amounts = outputs
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<u64>()
                            .map_err(|error| StepError::InvalidArgument {
                                message: format!("Invalid withdraw output amount '{s}': {error}"),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if amounts.is_empty() {
                    return Err(StepError::InvalidArgument {
                        message: format!(
                            "Atomic withdraw '{alias}' must list at least one output amount"
                        ),
                    });
                }
                Ok((alias.clone(), amounts))
            }
            _ => invalid_table_row("Atomic withdraw", &["withdraw", "outputs"], row.len()),
        },
    )
}
fn parse_turn_cell(value: &str) -> Result<bool, StepError> {
    match value.to_uppercase().trim() {
        "OUR_TURN" => Ok(true),
        "NOT_OUR_TURN" => Ok(false),
        _ => Err(StepError::InvalidArgument {
            message: format!(
                "Zone sequencing state `turn_to_write` must be `OUR_TURN` or `NOT_OUR_TURN`, got `{value}`"
            ),
        }),
    }
}
