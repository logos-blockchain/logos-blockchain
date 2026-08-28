use std::collections::BTreeMap;

use cucumber::{gherkin::Step, given, when};
use lb_testing_framework::get_reserved_available_tcp_port;

use crate::cucumber::{
    error::{StepError, StepResult},
    steps::{
        parse_steps::{
            invalid_table_row, parse_required_bool_cell, parse_required_string_cell,
            parse_table_rows,
        },
        tokio_console::profile::{TokioConsoleProfile, TokioConsoleProfileNode},
    },
    world::CucumberWorld,
};

const DEFAULT_BASE_PORT: u16 = 16_669;

pub(super) struct TokioConsoleResourcesRow {
    pub node_name: String,
    pub record_raw: bool,
}

#[given("I will have tokio console profile nodes:")]
#[when("I will have tokio console profile nodes:")]
fn step_tokio_console_profile_nodes(world: &mut CucumberWorld, step: &Step) -> StepResult {
    if world.cluster.local_cluster.is_some() {
        return Err(StepError::LogicalError {
            message: format!("'{}' must be precede cluster definition", step.value),
        });
    }

    let rows = tokio_console_rows(step)?;

    let profile_nodes: BTreeMap<String, TokioConsoleProfileNode> = rows
        .into_iter()
        .map(|row| {
            (
                row.node_name,
                TokioConsoleProfileNode {
                    port: get_reserved_available_tcp_port().unwrap_or(DEFAULT_BASE_PORT),
                    record_raw: row.record_raw,
                },
            )
        })
        .collect();
    world.tokio_console_profile = TokioConsoleProfile { profile_nodes };

    Ok(())
}

pub(super) fn tokio_console_rows(step: &Step) -> Result<Vec<TokioConsoleResourcesRow>, StepError> {
    let rows = parse_table_rows(
        step,
        &["node_name", "record_raw"],
        "Tokio console",
        parse_node_resource_row,
    )?;

    Ok(rows)
}

fn parse_node_resource_row(row: &[String]) -> Result<TokioConsoleResourcesRow, StepError> {
    match row {
        [node_name, record_raw] => {
            let node_name = parse_required_string_cell(node_name, "node_name", "Tokio console")?;
            let record_raw = parse_required_bool_cell(record_raw, "record_raw", "Tokio console")?;

            Ok(TokioConsoleResourcesRow {
                node_name,
                record_raw,
            })
        }
        _ => invalid_table_row("Tokio console", &["node_name", "record_raw"], row.len()),
    }
}
