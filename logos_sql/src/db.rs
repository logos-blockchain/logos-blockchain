//! Participant-local `SQLite` databases owned by the `λSQL` runtime.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use bincode::Options as _;
use lb_zone_sdk::sequencer::SequencerCheckpoint;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension as _, Row,
    hooks::{AuthAction, AuthContext, Authorization},
    params, params_from_iter,
};

use crate::{
    error::Error,
    protocol::{EncodedWrite, Transaction, TxId},
};

const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LIVE_DATABASE_FILE: &str = "LIVE.db";
const CONTROL_DATABASE_FILE: &str = "control.db";

// Stores locally committed writes and their exact channel payload. At most one
// write may be waiting for ZoneSDK publication.
const LIVE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_pending_write (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        tx_id BLOB NOT NULL UNIQUE CHECK (length(tx_id) = 32),
        payload BLOB NOT NULL
    ) STRICT;
";

// Stores the participant-local ZoneSDK checkpoint independently of live state.
const CONTROL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        checkpoint BLOB
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
    live: Connection,
    control: Connection,
    live_path: PathBuf,
}

impl Databases {
    /// Opens or creates the participant state under `directory`.
    pub(crate) fn open(directory: &Path) -> Result<Self, Error> {
        fs::create_dir_all(directory)?;

        let live_path = directory.join(LIVE_DATABASE_FILE);
        let control_path = directory.join(CONTROL_DATABASE_FILE);

        let live = open_writer(&live_path)?;
        let control = open_writer(&control_path)?;

        live.execute_batch(LIVE_SCHEMA)?;
        control.execute_batch(CONTROL_SCHEMA)?;
        control.execute(INITIALIZE_CONTROL_STATE, [])?;

        Ok(Self {
            live,
            control,
            live_path,
        })
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

        // Application SQL runs inside the transaction that also records its
        // pending publication. Keep it from escaping that transaction.
        db_transaction.authorizer(Some(authorize_application_sql));

        let apply_result = transaction.statements().iter().try_for_each(|statement| {
            // TODO: Capture nondeterministic function results and include them
            // in the transaction published to other participants.
            db_transaction.execute(statement.sql(), params_from_iter(statement.params()))?;

            Ok::<_, Error>(())
        });

        db_transaction.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        apply_result?;

        db_transaction.execute(
            INSERT_PENDING_WRITE,
            params![encoded.tx_id.as_ref(), encoded.payload],
        )?;
        db_transaction.commit()?;

        Ok(encoded.tx_id)
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

// Application statements share a transaction with the pending publication
// record, so they cannot take over transaction or connection management.
const fn authorize_application_sql(context: AuthContext<'_>) -> Authorization {
    let denied = matches!(
        context.action,
        AuthAction::Unknown { .. }
            | AuthAction::Transaction { .. }
            | AuthAction::Savepoint { .. }
            | AuthAction::Attach { .. }
            | AuthAction::Detach { .. }
            | AuthAction::Pragma { .. }
    );

    if denied {
        Authorization::Deny
    } else {
        Authorization::Allow
    }
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
    use tempfile::TempDir;

    use super::Databases;
    use crate::{
        error::Error,
        local_write,
        protocol::{EncodedWrite, Statement, Transaction, Value},
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
        let transaction = transaction(sql);
        let encoded = encoded_write(&transaction);

        let error = db
            .commit_local_write(&transaction, &encoded)
            .expect_err("application SQL should be rejected");

        assert!(matches!(error, Error::Database(_)));
        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );
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
