//! Participant-local `SQLite` databases owned by the `λSQL` runtime.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use bincode::Options as _;
use lb_zone_sdk::{node_types::MsgId, sequencer::SequencerCheckpoint};
use rusqlite::{
    Connection, ErrorCode as SqliteErrorCode, OpenFlags, OptionalExtension as _, Row,
    hooks::{AuthAction, AuthContext, Authorization},
    params, params_from_iter,
};

use crate::{
    error::Error,
    protocol::{ChannelInscription, EncodedWrite, Transaction, TxId},
};

const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LIB_DATABASE_FILE: &str = "LIB.db";
const LIVE_DATABASE_FILE: &str = "LIVE.db";
const CONTROL_DATABASE_FILE: &str = "control.db";
const RESERVED_OBJECT_PREFIX: &str = "__logos_sql_";

// Present in both state databases so replicated SQL observes the same schema.
// Only LIVE.db stores a row, committed atomically with the local write.
const PENDING_WRITE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_pending_write (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        tx_id BLOB NOT NULL UNIQUE CHECK (length(tx_id) = 32),
        payload BLOB NOT NULL
    ) STRICT;
";

// Records the SQL transactions whose effects exist in each database. The
// marker commits with the effects, making channel-event replay idempotent.
const APPLIED_WRITE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_applied_writes (
        tx_id BLOB PRIMARY KEY CHECK (length(tx_id) = 32),
        transaction_digest BLOB NOT NULL CHECK (length(transaction_digest) = 32)
    ) STRICT;
";

// Stores participant-local progress and rejected channel writes independently
// of replicated database state.
const CONTROL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        checkpoint BLOB
    ) STRICT;

    CREATE TABLE IF NOT EXISTS __logos_sql_rejected_writes (
        this_msg BLOB PRIMARY KEY CHECK (length(this_msg) = 32),
        tx_id BLOB CHECK (tx_id IS NULL OR length(tx_id) = 32),
        reason TEXT NOT NULL
    ) STRICT;
";

const INITIALIZE_CONTROL_STATE: &str = "
    INSERT OR IGNORE INTO __logos_sql_state (singleton, checkpoint)
    VALUES (1, NULL)
";

const SELECT_CHECKPOINT: &str = "
    SELECT checkpoint
    FROM __logos_sql_state
    WHERE singleton = 1
";

const UPDATE_CHECKPOINT: &str = "
    UPDATE __logos_sql_state
    SET checkpoint = ?1
    WHERE singleton = 1
";

const INSERT_REJECTED_WRITE: &str = "
    INSERT INTO __logos_sql_rejected_writes (this_msg, tx_id, reason)
    VALUES (?1, ?2, ?3)
    ON CONFLICT (this_msg) DO NOTHING
";
const INSERT_PENDING_WRITE: &str = "
    INSERT INTO __logos_sql_pending_write (singleton, tx_id, payload)
    VALUES (1, ?1, ?2)
";

const SELECT_PENDING_PUBLISH: &str = "
    SELECT tx_id, payload
    FROM __logos_sql_pending_write
    WHERE singleton = 1
";

const MARK_PUBLISH_COMPLETE: &str = "
    DELETE FROM __logos_sql_pending_write
    WHERE singleton = 1 AND tx_id = ?1
";

const SELECT_APPLIED_WRITE: &str = "
    SELECT transaction_digest
    FROM __logos_sql_applied_writes
    WHERE tx_id = ?1
";

const INSERT_APPLIED_WRITE: &str = "
    INSERT INTO __logos_sql_applied_writes (tx_id, transaction_digest)
    VALUES (?1, ?2)
";

const WRITER_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = FULL;
";

const FOREIGN_KEYS_PRAGMA: &str = "PRAGMA foreign_keys = ON;";

/// Raw database representation of a write waiting for publication.
struct StoredPendingPublish {
    tx_id: Vec<u8>,
    payload: Vec<u8>,
}

impl StoredPendingPublish {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            tx_id: row.get(0)?,
            payload: row.get(1)?,
        })
    }
}

/// A write committed to `LIVE.db` but not yet present in the persisted
/// `ZoneSDK` checkpoint.
pub struct PendingPublish {
    pub tx_id: TxId,
    pub payload: Vec<u8>,
}

/// Owns the participant-local database connections.
pub struct Databases {
    lib: Connection,
    live: Connection,
    control: Connection,
    lib_path: PathBuf,
    live_path: PathBuf,
}

impl Databases {
    /// Opens or creates the participant state under `directory`.
    pub(crate) fn open(directory: &Path) -> Result<Self, Error> {
        fs::create_dir_all(directory)?;

        let lib_path = directory.join(LIB_DATABASE_FILE);
        let live_path = directory.join(LIVE_DATABASE_FILE);
        let control_path = directory.join(CONTROL_DATABASE_FILE);

        let lib = open_writer(&lib_path)?;
        let live = open_writer(&live_path)?;
        let control = open_writer(&control_path)?;

        for connection in [&lib, &live] {
            connection.execute_batch(PENDING_WRITE_SCHEMA)?;
            connection.execute_batch(APPLIED_WRITE_SCHEMA)?;
        }

        control.execute_batch(CONTROL_SCHEMA)?;
        control.execute(INITIALIZE_CONTROL_STATE, [])?;

        Ok(Self {
            lib,
            live,
            control,
            lib_path,
            live_path,
        })
    }

    pub(crate) fn lib_path(&self) -> &Path {
        &self.lib_path
    }

    pub(crate) fn live_path(&self) -> &Path {
        &self.live_path
    }

    pub(crate) fn load_checkpoint(&self) -> Result<Option<SequencerCheckpoint>, Error> {
        let bytes = self
            .control
            .query_row(SELECT_CHECKPOINT, [], |row| {
                row.get::<_, Option<Vec<u8>>>(0)
            })
            .optional()?
            .flatten();

        bytes
            .map(|bytes| checkpoint_options().deserialize(&bytes))
            .transpose()
            .map_err(Error::from)
    }

    pub(crate) fn persist_checkpoint(
        &mut self,
        checkpoint: &SequencerCheckpoint,
    ) -> Result<(), Error> {
        let encoded = checkpoint_options().serialize(checkpoint)?;
        let transaction = self.control.transaction()?;

        transaction.execute(UPDATE_CHECKPOINT, [encoded])?;
        transaction.commit()?;

        Ok(())
    }

    /// Records a channel write that every replica must skip.
    ///
    /// Keeping the rejection in participant-local state allows replay to
    /// advance past invalid input and preserves the outcome for later
    /// application reporting. `tx_id` is absent when the payload could not be
    /// decoded.
    pub(crate) fn record_rejected_write(
        &self,
        this_msg: MsgId,
        tx_id: Option<TxId>,
        reason: &str,
    ) -> Result<(), Error> {
        let tx_id = tx_id.map(<[u8; 32]>::from);
        let tx_id = tx_id.as_ref().map(<[u8; 32]>::as_slice);

        self.control.execute(
            INSERT_REJECTED_WRITE,
            params![this_msg.as_ref(), tx_id, reason],
        )?;

        Ok(())
    }
    /// Commits application effects and their pending publish record together in
    /// `LIVE.db`.
    pub(crate) fn commit_local_write(
        &mut self,
        transaction: &Transaction,
        encoded: &EncodedWrite,
    ) -> Result<TxId, Error> {
        if self.pending_publish()?.is_some() {
            return Err(Error::PublishPending);
        }

        let db_transaction = self.live.transaction()?;

        // TODO: Capture nondeterministic function results and include them in
        // the transaction published to other participants.
        apply_statements(&db_transaction, transaction)?;

        let transaction_digest = transaction.digest();

        db_transaction.execute(
            INSERT_APPLIED_WRITE,
            params![encoded.tx_id.as_ref(), transaction_digest],
        )?;

        db_transaction.execute(
            INSERT_PENDING_WRITE,
            params![encoded.tx_id.as_ref(), encoded.payload],
        )?;
        db_transaction.commit()?;

        Ok(encoded.tx_id)
    }

    /// Applies a newly adopted channel write to the live database.
    pub(crate) fn apply_adopted_write(&mut self, write: &ChannelInscription) -> Result<(), Error> {
        apply_channel_write(&mut self.live, write)
    }

    /// Applies a finalized channel write to finalized and live state.
    ///
    /// Applying to `LIVE.db` as well covers writes first discovered through
    /// finalized backfill. Writes already applied locally or while adopted
    /// are skipped using their `TxId`.
    pub(crate) fn apply_finalized_write(
        &mut self,
        write: &ChannelInscription,
    ) -> Result<(), Error> {
        let transaction_digest = write.transaction.digest();

        is_write_applied(&self.lib, write.tx_id, &transaction_digest)?;
        is_write_applied(&self.live, write.tx_id, &transaction_digest)?;

        // LIB and LIVE are separate SQLite files. If LIVE fails after LIB
        // commits, the checkpoint remains behind. Redelivery then skips LIB
        // using its applied marker and completes LIVE.
        apply_channel_write(&mut self.lib, write)?;
        apply_channel_write(&mut self.live, write)
    }

    #[cfg(test)]
    pub(crate) fn rejected_write_count(&self) -> Result<i64, Error> {
        self.control
            .query_row(
                "SELECT count(*) FROM __logos_sql_rejected_writes",
                [],
                |row| row.get(0),
            )
            .map_err(Error::from)
    }

    pub(crate) fn pending_publish(&self) -> Result<Option<PendingPublish>, Error> {
        let record = self
            .live
            .query_row(SELECT_PENDING_PUBLISH, [], StoredPendingPublish::from_row)
            .optional()?;

        let Some(record) = record else {
            return Ok(None);
        };

        let tx_id = decode_tx_id(record.tx_id)?;

        Ok(Some(PendingPublish {
            tx_id,
            payload: record.payload,
        }))
    }

    pub(crate) fn mark_publish_complete(&self, tx_id: TxId) -> Result<(), Error> {
        let changed = self.live.execute(MARK_PUBLISH_COMPLETE, [tx_id.as_ref()])?;

        if changed != 1 {
            return Err(Error::InvalidLocalState(
                "pending publish record is missing",
            ));
        }

        Ok(())
    }

    pub(crate) fn open_reader(path: &Path) -> Result<Connection, Error> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;

        configure_connection(&conn)?;

        Ok(conn)
    }
}

fn open_writer(path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open(path)?;

    configure_connection(&conn)?;
    conn.execute_batch(WRITER_PRAGMAS)?;

    Ok(conn)
}

fn configure_connection(conn: &Connection) -> Result<(), Error> {
    conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
    conn.execute_batch(FOREIGN_KEYS_PRAGMA)?;

    Ok(())
}

fn apply_channel_write(
    connection: &mut Connection,
    write: &ChannelInscription,
) -> Result<(), Error> {
    let transaction_digest = write.transaction.digest();
    let db_transaction = connection.transaction()?;

    if is_write_applied(&db_transaction, write.tx_id, &transaction_digest)? {
        return Ok(());
    }

    if let Err(error) = apply_statements(&db_transaction, &write.transaction) {
        return match error {
            Error::Database(error) if is_deterministic_sql_error(&error) => {
                Err(Error::RejectedSql(error))
            }
            error => Err(error),
        };
    }

    db_transaction.execute(
        INSERT_APPLIED_WRITE,
        params![write.tx_id.as_ref(), transaction_digest],
    )?;
    db_transaction.commit()?;

    Ok(())
}

fn is_write_applied(
    connection: &Connection,
    tx_id: TxId,
    transaction_digest: &[u8; 32],
) -> Result<bool, Error> {
    let stored_digest = connection
        .query_row(SELECT_APPLIED_WRITE, [tx_id.as_ref()], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()?;

    if let Some(stored_digest) = stored_digest {
        if stored_digest.as_slice() != transaction_digest {
            return Err(Error::InvalidPayload(
                "transaction id was reused for different content",
            ));
        }

        return Ok(true);
    }

    Ok(false)
}

// Only failures determined by the received statement and parameters are safe
// to skip. Storage, locking, and other local failures must halt replay so the
// same channel position can be retried without diverging from other replicas.
fn is_deterministic_sql_error(error: &rusqlite::Error) -> bool {
    if error.sqlite_error().is_some_and(|error| {
        matches!(
            error.extended_code,
            rusqlite::ffi::SQLITE_ERROR_RETRY | rusqlite::ffi::SQLITE_ERROR_SNAPSHOT
        )
    }) {
        return false;
    }

    match error.sqlite_error_code() {
        Some(
            SqliteErrorCode::Unknown
            | SqliteErrorCode::TooBig
            | SqliteErrorCode::ConstraintViolation
            | SqliteErrorCode::TypeMismatch
            | SqliteErrorCode::AuthorizationForStatementDenied
            | SqliteErrorCode::ParameterOutOfRange,
        ) => true,
        Some(_) => false,
        None => matches!(
            error,
            rusqlite::Error::NulError(_)
                | rusqlite::Error::InvalidParameterName(_)
                | rusqlite::Error::ExecuteReturnedResults
                | rusqlite::Error::InvalidFunctionParameterType(_, _)
                | rusqlite::Error::UserFunctionError(_)
                | rusqlite::Error::ToSqlConversionFailure(_)
                | rusqlite::Error::InvalidQuery
                | rusqlite::Error::UnwindingPanic
                | rusqlite::Error::GetAuxWrongType
                | rusqlite::Error::MultipleStatement
                | rusqlite::Error::InvalidParameterCount(_, _)
        ),
    }
}

fn apply_statements(
    db_transaction: &rusqlite::Transaction<'_>,
    transaction: &Transaction,
) -> Result<(), Error> {
    db_transaction.authorizer(Some(authorize_application_sql));

    let result = transaction.statements().iter().try_for_each(|statement| {
        db_transaction.execute(statement.sql(), params_from_iter(statement.params()))?;

        Ok::<_, Error>(())
    });

    db_transaction.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);

    result
}

// λSQL owns the surrounding transaction and its bookkeeping tables, so
// application statements cannot modify either. Temporary objects are also
// denied because they exist only for one connection and cannot be replicated.
fn authorize_application_sql(context: AuthContext<'_>) -> Authorization {
    let denied = matches!(
        context.action,
        AuthAction::Unknown { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::Pragma { .. }
    ) || is_temporary_object_action(context.action)
        || action_uses_reserved_name(context.action);

    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
}

const fn is_temporary_object_action(action: AuthAction<'_>) -> bool {
    matches!(
        action,
        AuthAction::CreateTempIndex { .. }
            | AuthAction::CreateTempTable { .. }
            | AuthAction::CreateTempTrigger { .. }
            | AuthAction::CreateTempView { .. }
            | AuthAction::DropTempIndex { .. }
            | AuthAction::DropTempTable { .. }
            | AuthAction::DropTempTrigger { .. }
            | AuthAction::DropTempView { .. }
    )
}

fn action_uses_reserved_name(action: AuthAction<'_>) -> bool {
    match action {
        AuthAction::CreateIndex {
            index_name,
            table_name,
        }
        | AuthAction::DropIndex {
            index_name,
            table_name,
        } => is_reserved_name(index_name) || is_reserved_name(table_name),
        AuthAction::CreateTrigger {
            trigger_name,
            table_name,
        }
        | AuthAction::DropTrigger {
            trigger_name,
            table_name,
        } => is_reserved_name(trigger_name) || is_reserved_name(table_name),
        AuthAction::CreateTable { table_name }
        | AuthAction::Delete { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::Insert { table_name }
        | AuthAction::Read { table_name, .. }
        | AuthAction::Update { table_name, .. }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::Analyze { table_name }
        | AuthAction::CreateVtable { table_name, .. }
        | AuthAction::DropVtable { table_name, .. } => is_reserved_name(table_name),
        AuthAction::CreateView { view_name } | AuthAction::DropView { view_name } => {
            is_reserved_name(view_name)
        }
        AuthAction::Reindex { index_name } => is_reserved_name(index_name),
        _ => false,
    }
}

fn is_reserved_name(name: &str) -> bool {
    name.get(..RESERVED_OBJECT_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_OBJECT_PREFIX))
}

fn decode_tx_id(bytes: Vec<u8>) -> Result<TxId, Error> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::InvalidLocalState("stored transaction id is malformed"))?;

    Ok(bytes.into())
}

fn checkpoint_options() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_little_endian()
        .with_fixint_encoding()
        .reject_trailing_bytes()
}

#[cfg(test)]
mod tests {
    use lb_zone_sdk::{
        node_types::{HeaderId, MsgId, Slot},
        sequencer::SequencerCheckpoint,
    };
    use rusqlite::types::Value;
    use tempfile::TempDir;

    use super::Databases;
    use crate::{
        error::Error,
        local_write,
        protocol::{ChannelInscription, EncodedWrite, Statement, Transaction, TxId},
    };

    fn checkpoint(byte: u8, slot: u64) -> SequencerCheckpoint {
        SequencerCheckpoint {
            last_msg_id: MsgId::root(),
            pending_txs: Vec::new(),
            lib: HeaderId::from([byte; 32]),
            lib_slot: Slot::from(slot),
            channel_notes: Vec::new(),
        }
    }

    fn insert(value: &str) -> Transaction {
        Transaction::new(vec![
            Statement::new(
                "INSERT INTO items(value) VALUES (?1)".to_owned(),
                vec![Value::Text(value.to_owned())],
            )
            .expect("statement should be valid"),
        ])
        .expect("transaction should be valid")
    }

    fn transaction(sql: &str) -> Transaction {
        Transaction::new(vec![
            Statement::new(sql.to_owned(), Vec::new()).expect("statement should be valid"),
        ])
        .expect("transaction should be valid")
    }

    fn encoded_write(transaction: &Transaction) -> EncodedWrite {
        EncodedWrite::new(transaction).expect("write should encode")
    }

    fn assert_application_sql_rejected(sql: &str) {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value INTEGER)", [])
            .expect("application table should be created");

        let transaction = transaction(sql);
        let encoded = encoded_write(&transaction);

        let error = db
            .commit_local_write(&transaction, &encoded)
            .expect_err("application SQL should be rejected");

        assert!(matches!(
            error,
            Error::Database(ref error)
                if error.sqlite_error_code()
                    == Some(rusqlite::ErrorCode::AuthorizationForStatementDenied)
        ));
        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );
    }

    fn internal_schema(
        connection: &rusqlite::Connection,
    ) -> Vec<(String, String, String, Option<String>)> {
        let mut statement = connection
            .prepare(
                "SELECT type, name, tbl_name, sql
                 FROM sqlite_schema
                 WHERE name GLOB '__logos_sql_*'
                    OR tbl_name GLOB '__logos_sql_*'
                 ORDER BY type, name",
            )
            .expect("schema query should prepare");

        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("schema query should run")
            .collect::<Result<_, _>>()
            .expect("schema rows should decode")
    }

    #[test]
    fn checkpoint_survives_reopen() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let expected = checkpoint(7, 42);
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.persist_checkpoint(&expected)
            .expect("checkpoint should persist");
        drop(db);

        let db = Databases::open(dir.path()).expect("databases should reopen");
        let actual = db
            .load_checkpoint()
            .expect("checkpoint should load")
            .expect("checkpoint should exist");

        assert_eq!(actual.lib, expected.lib);
        assert_eq!(actual.lib_slot, expected.lib_slot);
    }

    #[test]
    fn application_write_and_pending_publish_commit_together() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
            .expect("application table should be created");

        let transaction = insert("hello");
        let encoded = encoded_write(&transaction);

        db.commit_local_write(&transaction, &encoded)
            .expect("write should commit");

        let count: i64 = db
            .live
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("row should be readable");

        assert_eq!(count, 1);
        assert_eq!(
            db.pending_publish()
                .expect("pending publish should load")
                .expect("pending publish should exist")
                .tx_id,
            encoded.tx_id
        );
    }

    #[test]
    fn state_databases_have_the_same_internal_schema() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let db = Databases::open(dir.path()).expect("databases should open");

        assert_eq!(internal_schema(&db.lib), internal_schema(&db.live));
    }

    #[test]
    fn repeated_write_is_a_new_transaction() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
            .expect("application table should be created");

        let transaction = insert("hello");
        let first_tx_id =
            local_write::commit(&mut db, &transaction).expect("first write should commit");
        db.mark_publish_complete(first_tx_id)
            .expect("first publication should complete");

        let second_tx_id =
            local_write::commit(&mut db, &transaction).expect("second write should commit");

        assert_ne!(second_tx_id, first_tx_id);

        let count: i64 = db
            .live
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("row should be readable");

        assert_eq!(count, 2);
    }

    #[test]
    fn reused_transaction_id_with_different_content_is_rejected() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
            .expect("application table should be created");

        let tx_id = TxId::from([7; 32]);
        let first = ChannelInscription {
            tx_id,
            transaction: insert("first"),
        };
        let conflicting = ChannelInscription {
            tx_id,
            transaction: insert("conflicting"),
        };

        db.apply_adopted_write(&first)
            .expect("first write should apply");

        let error = db
            .apply_adopted_write(&conflicting)
            .expect_err("conflicting write should be rejected");

        assert!(matches!(error, Error::InvalidPayload(_)));

        let value: String = db
            .live
            .query_row("SELECT value FROM items", [], |row| row.get(0))
            .expect("stored value should be readable");

        assert_eq!(value, "first");
    }

    #[test]
    fn conflicting_finalized_write_does_not_modify_lib() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        for connection in [&db.lib, &db.live] {
            connection
                .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
                .expect("application table should be created");
        }

        let tx_id = TxId::from([7; 32]);
        let first = ChannelInscription {
            tx_id,
            transaction: insert("first"),
        };
        let conflicting = ChannelInscription {
            tx_id,
            transaction: insert("conflicting"),
        };

        db.apply_adopted_write(&first)
            .expect("first write should apply to LIVE");

        let error = db
            .apply_finalized_write(&conflicting)
            .expect_err("conflicting finalized write should be rejected");

        assert!(matches!(error, Error::InvalidPayload(_)));

        let lib_count: i64 = db
            .lib
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("LIB row count should be readable");

        assert_eq!(lib_count, 0);
    }

    #[test]
    fn application_write_can_create_persistent_schema() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let transaction = transaction("CREATE TABLE items(value TEXT NOT NULL)");
        let encoded = encoded_write(&transaction);

        db.commit_local_write(&transaction, &encoded)
            .expect("schema write should commit");

        db.live
            .execute("INSERT INTO items(value) VALUES ('hello')", [])
            .expect("created table should be writable");
    }

    #[test]
    fn application_sql_cannot_control_wrapper_transaction() {
        for control in ["COMMIT", "ROLLBACK", "SAVEPOINT application"] {
            let dir = TempDir::new().expect("temporary directory should be created");
            let mut db = Databases::open(dir.path()).expect("databases should open");

            db.live
                .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
                .expect("application table should be created");

            let transaction = Transaction::new(vec![
                Statement::new(
                    "INSERT INTO items(value) VALUES ('hello')".to_owned(),
                    Vec::new(),
                )
                .expect("statement should be valid"),
                Statement::new(control.to_owned(), Vec::new()).expect("statement should be valid"),
            ])
            .expect("transaction should be valid");
            let encoded = encoded_write(&transaction);

            db.commit_local_write(&transaction, &encoded)
                .expect_err("transaction control should be rejected");

            let count: i64 = db
                .live
                .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
                .expect("row count should be readable");

            assert_eq!(count, 0);
            assert!(
                db.pending_publish()
                    .expect("pending publication should load")
                    .is_none()
            );
        }
    }

    #[test]
    fn application_sql_cannot_change_runtime_state() {
        for sql in [
            "PRAGMA synchronous = OFF",
            "ATTACH DATABASE ':memory:' AS other",
        ] {
            assert_application_sql_rejected(sql);
        }
    }

    #[test]
    fn application_sql_cannot_access_internal_objects() {
        for sql in [
            "DELETE FROM __logos_sql_applied_writes",
            "DROP TABLE __logos_sql_applied_writes",
            "ALTER TABLE __logos_sql_applied_writes RENAME TO application_table",
            "CREATE INDEX __logos_sql_index ON items(value)",
            "CREATE VIEW __logos_sql_view AS SELECT 1",
            "CREATE TRIGGER __logos_sql_trigger AFTER INSERT ON items BEGIN SELECT 1; END",
            "CREATE TABLE __LOGOS_SQL_mixed_case(value INTEGER)",
        ] {
            assert_application_sql_rejected(sql);
        }
    }

    #[test]
    fn application_sql_cannot_create_temporary_objects() {
        for sql in [
            "CREATE TEMP TABLE temporary_items(value INTEGER)",
            "CREATE TEMP VIEW temporary_items AS SELECT 1",
        ] {
            assert_application_sql_rejected(sql);
        }
    }

    #[test]
    fn reserved_name_check_accepts_non_ascii_identifiers() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let transaction = transaction("CREATE TABLE aaaaaaaaaa\u{65e5}(value INTEGER)");
        let encoded = encoded_write(&transaction);

        db.commit_local_write(&transaction, &encoded)
            .expect("non-reserved Unicode name should be accepted");
    }

    #[test]
    fn application_read_connection_is_read_only() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
            .expect("application table should be created");

        let read = Databases::open_reader(db.live_path()).expect("read connection should open");

        read.query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
            .expect("application table should be readable");

        assert!(
            read.execute("INSERT INTO items(value) VALUES ('hello')", [])
                .is_err()
        );
    }
}
