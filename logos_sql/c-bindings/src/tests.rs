use std::{
    ffi::{CStr, CString},
    ptr,
};

use rusqlite::Connection;
use serde_json::{Value, json};

use super::{
    Config, SqlStatus, SqlStatusCode, logos_sql_free_status, logos_sql_free_string, logos_sql_open,
    logos_sql_query, sql, string,
};

fn consume_status(status: SqlStatus) -> (SqlStatusCode, String) {
    let message = if status.message.is_null() {
        String::new()
    } else {
        // SAFETY: Tests pass owned statuses returned by these bindings.
        unsafe { CStr::from_ptr(status.message) }
            .to_str()
            .unwrap()
            .to_owned()
    };
    let code = status.code;
    // SAFETY: The message was read above and is returned exactly once.
    unsafe { logos_sql_free_status(status) };
    (code, message)
}

fn expect_ok<T>(result: Result<T, SqlStatus>) -> T {
    result.unwrap_or_else(|status| panic!("{:?}", consume_status(status)))
}

fn query(query: &str, parameters: &Value, max_rows: u32) -> Value {
    let connection = Connection::open_in_memory().unwrap();
    let result = expect_ok(sql::query(
        &connection,
        query,
        &parameters.to_string(),
        max_rows,
        1024,
    ));
    serde_json::to_value(result).unwrap()
}

#[test]
fn query_preserves_sqlite_types() {
    let values = json!([
        {"type": "null"},
        {"type": "integer", "value": "9223372036854775807"},
        {"type": "real", "value": 1.25},
        {"type": "text", "value": "hello\u{0}world"},
        {"type": "blob", "value": "00ff80"}
    ]);
    let result = query(
        "SELECT ?1 AS n, ?2 AS i, ?3 AS r, ?4 AS t, ?5 AS b",
        &values,
        10,
    );

    assert_eq!(result["columns"], json!(["n", "i", "r", "t", "b"]));
    assert_eq!(result["rows"][0], values);
    assert_eq!(result["truncated"], false);
}

#[test]
fn query_reports_truncation_without_claiming_an_exact_fit_is_truncated() {
    for (limit, truncated) in [(1, true), (2, false)] {
        let result = query("SELECT 1 UNION ALL SELECT 2", &json!([]), limit);
        assert_eq!(result["rows"].as_array().unwrap().len(), limit as usize);
        assert_eq!(result["truncated"], truncated);
    }
}

#[test]
fn queries_allow_schema_inspection_but_cannot_write_or_attach() {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute("CREATE TABLE items(value TEXT)", [])
        .unwrap();
    let result = expect_ok(sql::query(
        &connection,
        "PRAGMA table_info(items)",
        "[]",
        10,
        1024,
    ));
    assert_eq!(
        serde_json::to_value(result).unwrap()["rows"][0][1]["value"],
        "value"
    );

    for query in [
        "CREATE TABLE injected(value)",
        "ATTACH ':memory:' AS other",
        "PRAGMA writable_schema = ON",
        "SELECT 1; SELECT 2",
    ] {
        let connection = Connection::open_in_memory().unwrap();
        let error = sql::query(&connection, query, "[]", 10, 1024)
            .err()
            .expect(query);
        consume_status(error);
    }
}

#[test]
fn query_limits_stop_unbounded_results_and_work() {
    for query in [
        "SELECT zeroblob(17000000)",
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x + 1 FROM n) SELECT sum(x) FROM n",
    ] {
        let connection = Connection::open_in_memory().unwrap();
        let error = sql::query(&connection, query, "[]", 10, 1024)
            .err()
            .expect(query);
        consume_status(error);
    }
}

#[test]
fn query_respects_the_callers_byte_limit() {
    let connection = Connection::open_in_memory().unwrap();

    let error = sql::query(&connection, "SELECT 1", "[]", 10, 0)
        .err()
        .unwrap();
    assert_eq!(consume_status(error).0, SqlStatusCode::SqlInvalidArgument);

    let error = sql::query(&connection, "SELECT 1", "[]", 10, 1)
        .err()
        .unwrap();
    let (code, message) = consume_status(error);
    assert_eq!(code, SqlStatusCode::SqlFailure);
    assert!(message.contains("max_result_bytes (1)"));

    expect_ok(sql::query(&connection, "SELECT 1", "[]", 10, 1024));
}

#[test]
fn query_rejects_bad_parameters() {
    for invalid in [
        r#"[{"type":"blob","value":"zz"}]"#,
        r#"[{"type":"integer","value":"9223372036854775808"}]"#,
    ] {
        let connection = Connection::open_in_memory().unwrap();
        let error = sql::query(&connection, "SELECT ?1", invalid, 10, 1024)
            .err()
            .expect(invalid);
        assert_eq!(consume_status(error).0, SqlStatusCode::SqlInvalidArgument);
    }
}

#[test]
fn config_opens_read_only_and_rejects_writer_credentials() {
    let mut config = json!({
        "channel_id": "01".repeat(32),
        "node_url": "http://127.0.0.1:8080",
        "state_dir": "/tmp/logos-sql-config-test"
    });
    assert!(
        expect_ok(Config::parse(&config.to_string()))
            .writer
            .is_none()
    );

    config["writer"] = json!({"signing_key": "02".repeat(32)});
    let error = Config::parse(&config.to_string()).err().unwrap();
    assert_eq!(consume_status(error).0, SqlStatusCode::SqlInvalidArgument);
}

#[test]
fn ffi_errors_and_output_allocations_are_released() {
    // SAFETY: Null inputs are rejected before dereferencing them.
    let opened = unsafe { logos_sql_open(ptr::null(), 100) };
    assert!(opened.value.is_null());
    assert_eq!(
        consume_status(opened.error).0,
        SqlStatusCode::SqlNullPointer
    );

    let invalid_utf8 = [0xffu8, 0];
    // SAFETY: The array is NUL-terminated; the binding validates its encoding.
    let opened = unsafe { logos_sql_open(invalid_utf8.as_ptr().cast(), 100) };
    assert!(opened.value.is_null());
    assert_eq!(
        consume_status(opened.error).0,
        SqlStatusCode::SqlInvalidArgument
    );

    // SAFETY: The null handle is rejected, and the parameter string is static.
    let result = unsafe {
        logos_sql_query(
            ptr::null_mut(),
            0,
            c"SELECT 1".as_ptr(),
            c"[]".as_ptr(),
            1,
            1024,
        )
    };
    assert!(result.value.is_null());
    assert_eq!(
        consume_status(result.error).0,
        SqlStatusCode::SqlNullPointer
    );

    // SAFETY: Null is explicitly accepted by this cleanup function.
    unsafe { logos_sql_free_string(ptr::null_mut()) };

    let value = expect_ok(string("query result".to_owned()));
    // SAFETY: `string` returned a new, unreleased output allocation.
    unsafe { logos_sql_free_string(value) };

    let config = CString::new("{}").unwrap();
    // SAFETY: The configuration string remains alive for the call.
    let result = unsafe { logos_sql_open(config.as_ptr(), 100) };
    assert!(result.value.is_null());
    assert_eq!(
        consume_status(result.error).0,
        SqlStatusCode::SqlInvalidArgument
    );
}
