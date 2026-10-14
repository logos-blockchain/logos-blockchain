//! Blocking C bindings for Logos SQL. Call these from a worker thread, not the
//! UI thread. Each database owns its runtime; serialize calls using the same
//! handle. All returned allocations must be released through this library.

mod config;
mod error;
mod sql;

#[cfg(test)]
mod tests;

use std::{
    ffi::{CStr, CString, c_char},
    time::Duration,
};

use config::Config;
pub use error::{SqlStatus, SqlStatusCode};
use lb_c_common::{FfiError as _, FfiResult, memory};
use lb_c_macros::panic_to_error;
use logos_sql::LogosSql;
use serde::Serialize;
use tokio::{
    runtime::{Builder, Runtime},
    time::timeout,
};

/// An owned replica. Open separate handles and state directories for different
/// channels.
pub struct SqlDatabase {
    sql: LogosSql,
    runtime: Runtime,
}

#[repr(C)]
pub enum SqlReadView {
    SqlLatest = 0,
    SqlFinalized = 1,
}

pub type SqlDatabaseResult = FfiResult<*mut SqlDatabase, SqlStatus>;
pub type SqlStringResult = FfiResult<*mut c_char, SqlStatus>;

/// Starts a read-only replica and waits for initial synchronization, up to
/// `timeout_ms`. Configuration is described in the crate README. A failed or
/// timed-out start returns no handle and stops its runtime.
///
/// # Safety
/// `config_json` must point to a NUL-terminated UTF-8 string for the duration
/// of the call.
#[panic_to_error(SqlStatus)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logos_sql_open(
    config_json: *const c_char,
    timeout_ms: u64,
) -> SqlDatabaseResult {
    let result = (|| {
        // SAFETY: The caller keeps this NUL-terminated input alive during the call.
        let config = Config::parse(unsafe { text(config_json, "config_json") }?)?;
        if timeout_ms == 0 {
            return Err(SqlStatus::invalid("timeout_ms must be positive"));
        }

        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("logos-sql-ffi")
            .enable_all()
            .build()
            .map_err(|error| {
                SqlStatus::failure(format!("Failed to start the SQL runtime: {error}"))
            })?;

        let sql = runtime
            .block_on(async {
                timeout(Duration::from_millis(timeout_ms), LogosSql::start(config)).await
            })
            .map_err(|_| {
                SqlStatus::error(
                    SqlStatusCode::SqlTimeout,
                    "initial synchronization timed out",
                )
            })?
            .map_err(SqlStatus::from)?;

        Ok(SqlDatabase { sql, runtime })
    })();

    match result {
        Ok(database) => SqlDatabaseResult::from_value(database),
        Err(error) => SqlDatabaseResult::err(error),
    }
}

/// Stops replication and releases the handle, even if shutdown reports an
/// error.
///
/// # Safety
/// `database` must be a live handle returned by `logos_sql_open`, with no calls
/// in progress. Do not use it again after this call.
#[panic_to_error(SqlStatus)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logos_sql_close(database: *mut SqlDatabase) -> SqlStatus {
    if database.is_null() {
        return SqlStatus::null_pointer("database");
    }

    // SAFETY: The caller returns this live handle exactly once, with no active
    // calls.
    let SqlDatabase { sql, runtime } = *unsafe { Box::from_raw(database) };
    match runtime.block_on(sql.shutdown()) {
        Ok(()) => SqlStatus::default(),
        Err(error) => error.into(),
    }
}

/// Queries latest (0) or finalized (1) state. Returns JSON columns, typed rows,
/// and `truncated` when more than `max_rows` matched. Parameters are a JSON
/// array. Only reads and schema-inspection pragmas are allowed.
/// `max_result_bytes` must be positive and limits the serialized row data;
/// exceeding it returns an error rather than a partial result.
///
/// # Safety
/// `database` must be live and exclusively accessed during this call. String
/// pointers must be valid NUL-terminated UTF-8. Free the returned string with
/// `logos_sql_free_string`.
#[panic_to_error(SqlStatus)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logos_sql_query(
    database: *mut SqlDatabase,
    view: u32,
    query: *const c_char,
    parameters_json: *const c_char,
    max_rows: u32,
    max_result_bytes: usize,
) -> SqlStringResult {
    let result = (|| {
        // SAFETY: The caller keeps the handle alive and serializes its use.
        let database = unsafe { required(database, "database") }?;
        let connection = match view {
            0 => database.sql.read_connection(),
            1 => database.sql.finalized_read_connection(),
            _ => return Err(SqlStatus::invalid("unknown read view")),
        }
        .map_err(SqlStatus::from)?;

        // SAFETY: Both inputs are NUL-terminated and live for this call.
        let query = unsafe { text(query, "query") }?;
        // SAFETY: The caller keeps the parameter string alive for this call.
        let parameters = unsafe { text(parameters_json, "parameters_json") }?;
        let result = sql::query(&connection, query, parameters, max_rows, max_result_bytes)?;
        json(&result)
    })();

    result.into()
}

/// Releases an output string. Null is a no-op.
///
/// # Safety
/// A non-null pointer must be a string returned by this library, not already
/// freed.
#[panic_to_error(SqlStatus)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logos_sql_free_string(value: *mut c_char) {
    // SAFETY: The caller returns a string allocated by this library, or null.
    unsafe { memory::free_cstring(value) };
}

/// Releases a status message. Safe to call for success statuses too.
///
/// # Safety
/// The status must have been returned by this library and not already freed.
#[panic_to_error(SqlStatus)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn logos_sql_free_status(status: SqlStatus) {
    // SAFETY: The caller transfers ownership of the unreleased status message.
    unsafe { status.release() };
}

unsafe fn required<'a, T>(pointer: *const T, name: &str) -> Result<&'a T, SqlStatus> {
    // SAFETY: Callers guarantee a valid allocation lasting for the returned borrow.
    unsafe { pointer.as_ref() }.ok_or_else(|| SqlStatus::null_pointer(name))
}

unsafe fn text<'a>(pointer: *const c_char, name: &str) -> Result<&'a str, SqlStatus> {
    if pointer.is_null() {
        return Err(SqlStatus::null_pointer(name));
    }

    // SAFETY: Callers guarantee a NUL-terminated allocation lasting for this
    // borrow.
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .map_err(|error| SqlStatus::invalid(format!("Invalid UTF-8 in {name}: {error}")))
}

fn json(value: &impl Serialize) -> Result<*mut c_char, SqlStatus> {
    string(serde_json::to_string(value).map_err(|error| {
        SqlStatus::failure(format!("Failed to serialize query results: {error}"))
    })?)
}

fn string(value: String) -> Result<*mut c_char, SqlStatus> {
    CString::new(value)
        .map(CString::into_raw)
        .map_err(|error| SqlStatus::failure(error.to_string()))
}
