//! Cucumber entrypoints for Logos SQL scenarios.

use std::{
    num::{NonZeroU16, NonZeroUsize},
    time::Duration,
};

use cucumber::{gherkin::Step, given, then, when};
use logos_sql::PublicationConfig;

use super::{
    DatabaseKind,
    actions::{self, InstanceMode},
    assertions,
    tables::{instance_rows, write_rows},
};
use crate::{
    benchmarks::logos_sql::SqlWorkload,
    cucumber::{
        error::{StepError, StepResult},
        world::CucumberWorld,
    },
};

#[given("I start Logos SQL instances:")]
#[when("I start Logos SQL instances:")]
async fn step_start_logos_sql_instances(world: &mut CucumberWorld, step: &Step) -> StepResult {
    actions::start_instances(world, instance_rows(step)?, InstanceMode::default()).await
}

#[given(
    expr = "I start Logos SQL instances with a {int}% fee reserve, batches of {int} writes and a queue of {int} writes:"
)]
#[when(
    expr = "I start Logos SQL instances with a {int}% fee reserve, batches of {int} writes and a queue of {int} writes:"
)]
async fn step_start_logos_sql_instances_with_batching(
    world: &mut CucumberWorld,
    priority_fee_percent: u64,
    max_transactions: NonZeroU16,
    max_pending_writes: NonZeroUsize,
    step: &Step,
) -> StepResult {
    let mode = InstanceMode::Writer {
        priority_fee_percent,
        publication: PublicationConfig {
            max_transactions,
            max_pending_writes,
        },
    };

    actions::start_instances(world, instance_rows(step)?, mode).await
}

#[given("I start read-only Logos SQL instances:")]
#[when("I start read-only Logos SQL instances:")]
async fn step_start_read_only_logos_sql_instances(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    actions::start_instances(world, instance_rows(step)?, InstanceMode::ReadOnly).await
}

#[when(expr = "I stop Logos SQL instance {string}")]
async fn step_stop_logos_sql_instance(
    world: &mut CucumberWorld,
    instance_alias: String,
) -> StepResult {
    actions::stop_instance(world, &instance_alias).await
}

#[when(expr = "Logos SQL instance {string} executes write {string}:")]
async fn step_execute_logos_sql_write(
    world: &mut CucumberWorld,
    step: &Step,
    instance_alias: String,
    write_alias: String,
) -> StepResult {
    actions::execute_write(world, &instance_alias, write_alias, sql_docstring(step)?).await
}

#[when("the following Logos SQL writes execute concurrently:")]
async fn step_execute_logos_sql_writes_concurrently(
    world: &mut CucumberWorld,
    step: &Step,
) -> StepResult {
    actions::execute_writes_concurrently(world, write_rows(step)?).await
}

#[when(
    expr = "I benchmark Logos SQL writer {string} and replica {string} on sequencer {string} for {int} seconds with {int} byte rows"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_benchmark_logos_sql(
    world: &mut CucumberWorld,
    writer_alias: String,
    replica_alias: String,
    sequencer_alias: String,
    seconds: u64,
    payload_bytes: usize,
) -> StepResult {
    let workload = SqlWorkload {
        payload_bytes,
        measure_for: Duration::from_secs(seconds),
    };

    actions::benchmark(
        world,
        &writer_alias,
        &replica_alias,
        &sequencer_alias,
        workload,
    )
    .await
}

#[then(
    expr = "Logos SQL instance {string} has {int} rows in table {string} in its {logos_sql_database} database in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_table_row_count(
    world: &mut CucumberWorld,
    instance_alias: String,
    expected: usize,
    table: String,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    assertions::wait_for_table_row_count(
        world,
        &instance_alias,
        &table,
        expected,
        database,
        timeout_seconds,
    )
    .await
}

#[then(
    expr = "Logos SQL instances {string} and {string} agree on this {logos_sql_database} query in {int} seconds:"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_query_agreement(
    world: &mut CucumberWorld,
    step: &Step,
    left_alias: String,
    right_alias: String,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    assertions::wait_for_query_agreement(
        world,
        &left_alias,
        &right_alias,
        &sql_docstring(step)?,
        database,
        timeout_seconds,
    )
    .await
}

#[then(
    expr = "Logos SQL instance {string} returns text {string} from this {logos_sql_database} query in {int} seconds:"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_text_query_result(
    world: &mut CucumberWorld,
    step: &Step,
    instance_alias: String,
    expected: String,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    assertions::wait_for_text_query_result(
        world,
        &instance_alias,
        &sql_docstring(step)?,
        &expected,
        database,
        timeout_seconds,
    )
    .await
}

#[then(
    expr = "exactly one of Logos SQL writes {string} and {string} is displaced in {int} seconds"
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_one_write_is_displaced(
    world: &mut CucumberWorld,
    left_alias: String,
    right_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    assertions::wait_for_one_displaced_write(world, &left_alias, &right_alias, timeout_seconds)
        .await
}

#[then(expr = "Logos SQL write {string} is displaced in {int} seconds")]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "Cucumber step functions require the world as the first mutable argument"
)]
async fn step_write_is_displaced(
    world: &mut CucumberWorld,
    write_alias: String,
    timeout_seconds: u64,
) -> StepResult {
    assertions::wait_for_displaced_write(world, &write_alias, timeout_seconds).await
}

fn sql_docstring(step: &Step) -> Result<String, StepError> {
    let sql = step
        .docstring
        .as_deref()
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
        .ok_or_else(|| StepError::InvalidArgument {
            message: format!("step '{}' requires a non-empty SQL doc string", step.value),
        })?;

    Ok(sql.to_owned())
}
