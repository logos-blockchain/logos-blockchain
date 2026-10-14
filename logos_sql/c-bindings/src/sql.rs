use rusqlite::{
    Batch, Connection,
    fallible_iterator::FallibleIterator as _,
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    params_from_iter,
    types::{Value, ValueRef},
};
use serde::{Deserialize, Serialize};

use crate::SqlStatus;

// These limits apply only to interactive reads, not replicated transactions.
const MAX_SQL_BYTES: i32 = 16 * 1024 * 1024;
const MAX_QUERY_ROWS: u32 = 100_000;

/// Integer strings preserve all 64 bits in clients whose JSON numbers use
/// doubles.
#[derive(Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SqlValue {
    Null,
    Integer(String),
    Real(f64),
    Text(String),
    Blob(String),
}

impl SqlValue {
    fn into_value(self) -> Result<Value, SqlStatus> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Integer(value) => value
                .parse()
                .map(Value::Integer)
                .map_err(|error| SqlStatus::invalid(format!("Invalid integer parameter: {error}"))),
            Self::Real(value) => Ok(Value::Real(value)),
            Self::Text(value) => Ok(Value::Text(value)),
            Self::Blob(value) => hex::decode(value)
                .map(Value::Blob)
                .map_err(|error| SqlStatus::invalid(format!("Invalid blob parameter: {error}"))),
        }
    }

    fn from_value(value: ValueRef<'_>) -> Result<Self, SqlStatus> {
        match value {
            ValueRef::Null => Ok(Self::Null),
            ValueRef::Integer(value) => Ok(Self::Integer(value.to_string())),
            ValueRef::Real(value) if value.is_finite() => Ok(Self::Real(value)),
            ValueRef::Real(_) => Err(SqlStatus::failure(
                "non-finite real cannot be represented in JSON",
            )),
            ValueRef::Text(value) => String::from_utf8(value.to_vec())
                .map(Self::Text)
                .map_err(|error| SqlStatus::failure(error.to_string())),
            ValueRef::Blob(value) => Ok(Self::Blob(hex::encode(value))),
        }
    }
}

#[derive(Serialize)]
pub struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<SqlValue>>,
    truncated: bool,
}

pub fn query(
    connection: &Connection,
    sql: &str,
    parameters: &str,
    max_rows: u32,
    max_result_bytes: usize,
) -> Result<QueryResult, SqlStatus> {
    if max_rows == 0 || max_rows > MAX_QUERY_ROWS {
        return Err(SqlStatus::invalid("max_rows must be between 1 and 100000"));
    }
    if max_result_bytes == 0 {
        return Err(SqlStatus::invalid("max_result_bytes must be positive"));
    }

    let parameters: Vec<SqlValue> = serde_json::from_str(parameters)
        .map_err(|error| SqlStatus::invalid(format!("Invalid query parameters JSON: {error}")))?;
    let parameters = parameters
        .into_iter()
        .map(SqlValue::into_value)
        .collect::<Result<Vec<_>, _>>()?;

    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_SQL_BYTES)
        .map_err(|error| SqlStatus::failure(error.to_string()))?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES)
        .map_err(|error| SqlStatus::failure(error.to_string()))?;
    connection.authorizer(Some(authorize_read));

    // Stop expensive queries even when they produce no rows to count.
    let mut ticks = 0;
    connection.progress_handler(
        1000,
        Some(move || {
            ticks += 1;
            ticks >= 10_000
        }),
    );

    let mut batch = Batch::new(connection, sql);
    let mut statement = batch
        .next()
        .map_err(|error| SqlStatus::failure(format!("Failed to prepare query: {error}")))?
        .ok_or_else(|| SqlStatus::invalid("query is empty"))?;

    if batch
        .next()
        .map_err(|error| SqlStatus::failure(format!("Failed to prepare query: {error}")))?
        .is_some()
    {
        return Err(SqlStatus::invalid("query must contain one statement"));
    }

    let columns = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut result = QueryResult {
        columns,
        rows: Vec::new(),
        truncated: false,
    };
    let mut rows = statement
        .query(params_from_iter(parameters))
        .map_err(|error| SqlStatus::failure(format!("Failed to bind query parameters: {error}")))?;
    let mut result_bytes = 0;

    while let Some(row) = rows
        .next()
        .map_err(|error| SqlStatus::failure(format!("Failed to read query results: {error}")))?
    {
        if result.rows.len() == max_rows as usize {
            result.truncated = true;
            break;
        }

        let values = (0..result.columns.len())
            .map(|index| {
                SqlValue::from_value(
                    row.get_ref(index)
                        .map_err(|error| SqlStatus::failure(error.to_string()))?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        result_bytes += serde_json::to_vec(&values)
            .map_err(|error| SqlStatus::failure(format!("Failed to serialize query row: {error}")))?
            .len();
        if result_bytes > max_result_bytes {
            return Err(SqlStatus::failure(format!(
                "query result exceeds max_result_bytes ({max_result_bytes}); select fewer rows or columns, or increase the limit"
            )));
        }

        result.rows.push(values);
    }

    Ok(result)
}

fn authorize_read(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
            Authorization::Allow
        }
        AuthAction::Function { function_name }
            if !function_name.eq_ignore_ascii_case("load_extension") =>
        {
            Authorization::Allow
        }
        AuthAction::Pragma { pragma_name, .. }
            if [
                "table_info",
                "table_xinfo",
                "table_list",
                "index_list",
                "index_info",
                "index_xinfo",
                "foreign_key_list",
            ]
            .iter()
            .any(|name| pragma_name.eq_ignore_ascii_case(name)) =>
        {
            Authorization::Allow
        }
        _ => Authorization::Deny,
    }
}
