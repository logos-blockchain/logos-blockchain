use std::collections::HashMap;

use cucumber::gherkin::Step;

use crate::cucumber::error::StepError;

#[derive(Clone)]
pub(super) struct ConcurrentZoneMessageRow {
    pub sequencer_alias: String,
    pub message_alias: String,
    pub payload: Vec<u8>,
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

pub(super) fn zone_message_rows(step: &Step) -> Result<Vec<(String, Vec<u8>)>, StepError> {
    parse_zone_table_rows(step, &["alias", "data"], "Zone message", |row| match row {
        [alias, data] => Ok((alias.clone(), data.as_bytes().to_vec())),
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
                payload: data.as_bytes().to_vec(),
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
