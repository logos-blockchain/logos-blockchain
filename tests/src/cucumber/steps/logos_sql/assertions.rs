//! Polling assertions over replicated database state and write outcomes.

use std::time::Duration;

use rusqlite::{Connection, types::Value};
use tokio::time::{Instant, sleep};

use super::DatabaseKind;
use crate::cucumber::{
    error::{StepError, StepResult},
    world::CucumberWorld,
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub(super) async fn wait_for_table_row_count(
    world: &CucumberWorld,
    instance_alias: &str,
    table: &str,
    expected: usize,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    if !table
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(StepError::InvalidArgument {
            message: format!("invalid SQLite table name '{table}'"),
        });
    }

    let query = format!("SELECT COUNT(*) FROM \"{table}\"");
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        let observation = match query_rows(world, instance_alias, &query, database) {
            Ok(rows) => {
                let actual =
                    rows.first()
                        .and_then(|row| row.first())
                        .and_then(|value| match value {
                            Value::Integer(value) => usize::try_from(*value).ok(),
                            _ => None,
                        });

                if actual == Some(expected) {
                    return Ok(());
                }

                format!("returned {actual:?} rows")
            }
            Err(error) => error.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Logos SQL instance '{instance_alias}' did not reach {expected} rows in table '{table}' in its {} database: {observation}",
                    database.name()
                ),
            });
        }

        sleep(POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_query_agreement(
    world: &CucumberWorld,
    left_alias: &str,
    right_alias: &str,
    query: &str,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        let left = query_rows(world, left_alias, query, database);
        let right = query_rows(world, right_alias, query, database);

        let observation = match (left, right) {
            (Ok(left), Ok(right)) if left == right => return Ok(()),
            (Ok(left), Ok(right)) => {
                format!("left returned {left:?}; right returned {right:?}")
            }
            (Err(error), _) | (_, Err(error)) => error.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Logos SQL instances '{left_alias}' and '{right_alias}' did not agree on their {} query: {observation}",
                    database.name()
                ),
            });
        }

        sleep(POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_text_query_result(
    world: &CucumberWorld,
    instance_alias: &str,
    query: &str,
    expected: &str,
    database: DatabaseKind,
    timeout_seconds: u64,
) -> StepResult {
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let expected_rows = vec![vec![Value::Text(expected.to_owned())]];

    loop {
        let observation = match query_rows(world, instance_alias, query, database) {
            Ok(actual) if actual == expected_rows => return Ok(()),
            Ok(actual) => format!("returned {actual:?}"),
            Err(error) => error.to_string(),
        };

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "Logos SQL instance '{instance_alias}' did not return text '{expected}' from its {} query: {observation}",
                    database.name()
                ),
            });
        }

        sleep(POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_one_displaced_write(
    world: &CucumberWorld,
    left_alias: &str,
    right_alias: &str,
    timeout_seconds: u64,
) -> StepResult {
    let left = world.logos_sql.write(left_alias)?;
    let right = world.logos_sql.write(right_alias)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        let displaced = world.logos_sql.displaced_writes().await?;
        if displaced.contains(&left) ^ displaced.contains(&right) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!(
                    "expected exactly one of Logos SQL writes '{left_alias}' and '{right_alias}' to be displaced; observed {displaced:?}"
                ),
            });
        }

        sleep(POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_displaced_write(
    world: &CucumberWorld,
    write_alias: &str,
    timeout_seconds: u64,
) -> StepResult {
    let tx_id = world.logos_sql.write(write_alias)?;
    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    loop {
        if world.logos_sql.displaced_writes().await?.contains(&tx_id) {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(StepError::Timeout {
                message: format!("Logos SQL write '{write_alias}' was not reported as displaced"),
            });
        }

        sleep(POLL_INTERVAL).await;
    }
}

fn query_rows(
    world: &CucumberWorld,
    instance_alias: &str,
    query: &str,
    database: DatabaseKind,
) -> Result<Vec<Vec<Value>>, StepError> {
    let instance = world.logos_sql.instance(instance_alias)?;
    let connection = match database {
        DatabaseKind::Live => instance.read_connection()?,
        DatabaseKind::Finalized => instance.finalized_read_connection()?,
    };

    collect_rows(&connection, query).map_err(StepError::from)
}

fn collect_rows(connection: &Connection, query: &str) -> rusqlite::Result<Vec<Vec<Value>>> {
    let mut statement = connection.prepare(query)?;
    let column_count = statement.column_count();
    let rows = statement.query_map([], |row| {
        (0..column_count)
            .map(|column| row.get(column))
            .collect::<rusqlite::Result<Vec<Value>>>()
    })?;

    rows.collect()
}
