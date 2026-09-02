//! Parsing for the tables used by Logos SQL Cucumber steps.

use cucumber::gherkin::Step;

use crate::cucumber::{error::StepError, steps::parse_steps::parse_table_rows};

pub(super) struct InstanceRow {
    pub alias: String,
    pub sequencer: String,
}

pub(super) struct WriteRow {
    pub instance: String,
    pub write: String,
    pub sql: String,
}

pub(super) fn instance_rows(step: &Step) -> Result<Vec<InstanceRow>, StepError> {
    parse_table_rows(
        step,
        &["alias", "sequencer"],
        "Logos SQL instance",
        |row| match row {
            [alias, sequencer] if !alias.trim().is_empty() && !sequencer.trim().is_empty() => {
                Ok(InstanceRow {
                    alias: alias.clone(),
                    sequencer: sequencer.clone(),
                })
            }
            [_, _] => Err(StepError::InvalidArgument {
                message: "Logos SQL instance aliases and sequencers cannot be empty".to_owned(),
            }),
            _ => Err(StepError::InvalidArgument {
                message: format!(
                    "Logos SQL instance rows must have exactly 2 columns, got {}",
                    row.len()
                ),
            }),
        },
    )
}

pub(super) fn write_rows(step: &Step) -> Result<Vec<WriteRow>, StepError> {
    parse_table_rows(
        step,
        &["instance", "write", "sql"],
        "Logos SQL concurrent write",
        |row| match row {
            [instance, write, sql]
                if !instance.trim().is_empty()
                    && !write.trim().is_empty()
                    && !sql.trim().is_empty() =>
            {
                Ok(WriteRow {
                    instance: instance.clone(),
                    write: write.clone(),
                    sql: sql.clone(),
                })
            }
            [_, _, _] => Err(StepError::InvalidArgument {
                message: "Logos SQL concurrent write fields cannot be empty".to_owned(),
            }),
            _ => Err(StepError::InvalidArgument {
                message: format!(
                    "Logos SQL concurrent write rows must have exactly 3 columns, got {}",
                    row.len()
                ),
            }),
        },
    )
}
