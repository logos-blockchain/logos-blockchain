# Logos SQL C bindings

Build with `cargo build -p logos-sql-c`, or `nix build .#logos-sql-c` for the packaged library and header. The outputs are `liblogos_sql` and `logos_sql.h`. The node library is not required: Logos SQL connects to a node's HTTP API.

## Open a replica

Replicas are read-only. Pass a JSON configuration to `logos_sql_open`, together with an initial-sync timeout in milliseconds:

```json
{
  "channel_id": "<64 hex digits>",
  "node_url": "http://127.0.0.1:8080",
  "state_dir": "/path/to/replica"
}
```

Use a separate state directory for each replica; do not share it with another handle or process.

Calls are blocking. Run them on a worker thread, one call at a time per handle.

## Query data

`logos_sql_query(database, view, sql, parameters_json, max_rows, max_result_bytes)` reads through Logos SQL's own read-only connections. Use `SqlLatest` for the latest replicated state, including unfinalized changes, or `SqlFinalized` for finalized chain state.

The parameter array and returned cells use the same format:

```json
[
  {"type": "null"},
  {"type": "integer", "value": "9223372036854775807"},
  {"type": "real", "value": 1.25},
  {"type": "text", "value": "hello"},
  {"type": "blob", "value": "00ff80"}
]
```

Integers are decimal strings to preserve all 64 bits in JSON clients. Blobs are hex strings. Pass `[]` for a query with no parameters.

Results contain column names and rows, preserving column order and duplicate names:

```json
{
  "columns": ["id", "title"],
  "rows": [[{"type":"integer","value":"1"},{"type":"text","value":"First task"}]],
  "truncated": false
}
```

`max_rows` limits the number of returned rows; `truncated` indicates that more rows matched. Queries accept one read-only statement, including schema-inspection pragmas.

`max_result_bytes` sets a positive byte limit on serialized row data, excluding column names and the surrounding JSON. Exceeding it returns an error, not a partial result. For example, pass `16 * 1024 * 1024` for a 16 MiB row-data limit. SQLite values, rows and SQL text retain a separate 16 MiB limit regardless of the response limit.

## Ownership and errors

Check `result.error.code == SqlOk` before using `result.value`. Free returned strings with `logos_sql_free_string` and statuses with `logos_sql_free_status`. Call `logos_sql_close` once when finished; it releases the handle even if shutdown reports an error.
