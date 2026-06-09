use std::collections::{HashMap, HashSet};

use cucumber::gherkin::Step;
use lb_core::mantle::ops::channel::inscribe::Inscription;

use crate::{common::mantle_inscription::make_inscription, cucumber::error::StepError};

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
    let rows = parse_zone_table_rows(
        step,
        &[
            "node_name",
            "account_index",
            "wallet_name",
            "connected_to",
            "sequencers",
        ],
        "Zone node resources",
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
            let node_name = required_cell(node_name, "node_name")?;
            let account_index =
                account_index
                    .trim()
                    .parse()
                    .map_err(|error| StepError::InvalidArgument {
                        message: format!("Invalid zone account index '{account_index}': {error}"),
                    })?;
            let wallet_name = required_cell(wallet_name, "wallet_name")?;
            let connected_to = optional_cell(connected_to);
            let sequencers = comma_separated_aliases(sequencers, "sequencers")?;

            Ok(ZoneNodeResourcesRow {
                node_name,
                account_index,
                wallet_name,
                connected_to,
                sequencers,
            })
        }
        _ => invalid_zone_table_row(
            "Zone node resources",
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

fn required_cell(value: &str, name: &str) -> Result<String, StepError> {
    let value = value.trim();

    if value.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Zone node resources `{name}` cannot be empty"),
        });
    }

    Ok(value.to_owned())
}

fn optional_cell(value: &str) -> Option<String> {
    let value = value.trim();

    (!value.is_empty()).then(|| value.to_owned())
}

fn comma_separated_aliases(value: &str, name: &str) -> Result<Vec<String>, StepError> {
    let aliases = value
        .split(',')
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    if aliases.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Zone node resources `{name}` must list at least one alias"),
        });
    }

    Ok(aliases)
}

pub(super) fn zone_message_rows(step: &Step) -> Result<Vec<(String, Inscription)>, StepError> {
    parse_zone_table_rows(step, &["alias", "data"], "Zone message", |row| match row {
        [alias, data] => Ok((alias.clone(), make_inscription(data))),
        _ => invalid_zone_table_row("Zone message", &["alias", "data"], row.len()),
    })
}

pub(super) fn concurrent_zone_message_rows(
    step: &Step,
) -> Result<Vec<ConcurrentZoneMessageRow>, StepError> {
    parse_zone_table_rows(
        step,
        &["sequencer", "alias", "data"],
        "Concurrent zone message",
        |row| match row {
            [sequencer_alias, message_alias, data] => Ok(ConcurrentZoneMessageRow {
                sequencer_alias: sequencer_alias.clone(),
                message_alias: message_alias.clone(),
                payload: make_inscription(data),
            }),
            _ => invalid_zone_table_row(
                "Concurrent zone message",
                &["sequencer", "alias", "data"],
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
    parse_zone_table_rows(
        step,
        &["sequencer", "data_prefix"],
        "Generated zone message batch",
        |row| match row {
            [sequencer_alias, data_prefix] => Ok(GeneratedZoneMessageBatch {
                sequencer_alias: sequencer_alias.clone(),
                data_prefix: data_prefix.clone(),
            }),
            _ => invalid_zone_table_row(
                "Generated zone message batch",
                &["sequencer", "data_prefix"],
                row.len(),
            ),
        },
    )
}

pub(super) fn generated_zone_message_sequencers(step: &Step) -> Result<Vec<String>, StepError> {
    parse_zone_table_rows(
        step,
        &["sequencer"],
        "Generated zone message sequencer",
        |row| match row {
            [sequencer_alias] => Ok(sequencer_alias.clone()),
            _ => invalid_zone_table_row(
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
    parse_zone_table_rows(
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
                indexer: parse_bool_cell(indexer, "indexer")?,
                pending_submit_depth: pending_submit_depth.clone(),
                passive_republish_orphans: parse_bool_cell(
                    passive_republish_orphans,
                    "passive_republish_orphans",
                )?,
            }),
            _ => invalid_zone_table_row(
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
    parse_single_zone_table_row(
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
            _ => invalid_zone_table_row(
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
    parse_single_zone_table_row(
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
                )?,
            }),
            _ => invalid_zone_table_row(
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
    parse_zone_table_rows(
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
            _ => invalid_zone_table_row("Zone account balance", &["account", "balance"], row.len()),
        },
    )
}

pub(super) fn zone_balance_rows(step: &Step) -> Result<Vec<ZoneBalanceRow>, StepError> {
    parse_zone_table_rows(
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
            _ => invalid_zone_table_row(
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
    parse_zone_table_rows(
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
            _ => invalid_zone_table_row("Atomic withdraw", &["withdraw", "outputs"], row.len()),
        },
    )
}

pub(super) fn single_column_table(
    step: &Step,
    header_name: &str,
    description: &str,
) -> Result<Vec<String>, StepError> {
    parse_zone_table_rows(step, &[header_name], description, |row| match row {
        [value] => Ok(value.clone()),
        _ => invalid_zone_table_row(description, &[header_name], row.len()),
    })
}

fn parse_zone_table_rows<T>(
    step: &Step,
    headers: &[&str],
    description: &str,
    parse_row: impl Fn(&[String]) -> Result<T, StepError>,
) -> Result<Vec<T>, StepError> {
    let table = step.table.as_ref().ok_or(StepError::MissingTable)?;

    if table.rows.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("{description} must include a header row"),
        });
    }

    let header = table.rows[0].iter().map(String::as_str).collect::<Vec<_>>();
    if header.as_slice() != headers {
        return Err(StepError::InvalidArgument {
            message: format!("{description} must use `{}` header", headers.join("`, `")),
        });
    }

    table
        .rows
        .iter()
        .skip(1)
        .map(|row| parse_row(row))
        .collect()
}

fn parse_single_zone_table_row<T>(
    step: &Step,
    headers: &[&str],
    description: &str,
    parse_row: impl Fn(&[String]) -> Result<T, StepError>,
) -> Result<T, StepError> {
    let mut rows = parse_zone_table_rows(step, headers, description, parse_row)?;
    if rows.len() != 1 {
        return Err(StepError::InvalidArgument {
            message: format!("{description} must contain exactly one data row"),
        });
    }

    Ok(rows.remove(0))
}

fn parse_bool_cell(value: &str, column: &str) -> Result<bool, StepError> {
    match value.to_lowercase().trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(StepError::InvalidArgument {
            message: format!("Zone sequencer startup `{column}` must be `true` or `false`"),
        }),
    }
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

fn parse_usize_cell(value: &str, column: &str) -> Result<usize, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

fn parse_u32_cell(value: &str, column: &str) -> Result<u32, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

fn parse_u64_cell(value: &str, column: &str) -> Result<u64, StepError> {
    value.parse().map_err(|error| StepError::InvalidArgument {
        message: format!("Invalid `{column}` value `{value}`: {error}"),
    })
}

fn parse_list_cell(value: &str, column: &str) -> Result<Vec<String>, StepError> {
    let values = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if values.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Zone config transaction `{column}` must list at least one alias"),
        });
    }

    Ok(values)
}

fn invalid_zone_table_row<T>(
    description: &str,
    headers: &[&str],
    actual_columns: usize,
) -> Result<T, StepError> {
    Err(StepError::InvalidArgument {
        message: format!(
            "{description} rows must have exactly {} columns (`{}`), got {actual_columns}",
            headers.len(),
            headers.join("`, `"),
        ),
    })
}
