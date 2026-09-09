//! Participant-local `SQLite` databases owned by the `λSQL` runtime.

use std::{
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

use bincode::Options as _;
use lb_zone_sdk::{node_types::MsgId, sequencer::SequencerCheckpoint};
use rusqlite::{
    Connection, ErrorCode as SqliteErrorCode, OpenFlags, OptionalExtension as _, Row,
    backup::Backup,
    hooks::{AuthAction, AuthContext, Authorization},
    params, params_from_iter,
};

use crate::{
    error::Error,
    functions::FunctionOverrides,
    protocol::{ChannelInscription, EncodedWrite, Transaction, TxId},
};

const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LIB_DATABASE_FILE: &str = "LIB.db";
const LIVE_DATABASE_FILE: &str = "LIVE.db";
const CONTROL_DATABASE_FILE: &str = "control.db";
const REBUILD_DATABASE_FILE: &str = "LIVE.rebuild.db";
const BACKUP_PAGES_PER_STEP: i32 = 128;
const BACKUP_RETRY_DELAY: Duration = Duration::from_millis(10);
const RESERVED_OBJECT_PREFIX: &str = "__logos_sql_";

// Present in both state databases so replicated SQL observes the same schema.
// Only LIVE.db stores a row, committed atomically with the local write.
const PENDING_PUBLISH_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS __logos_sql_pending_publish (
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
        content_digest BLOB NOT NULL CHECK (length(content_digest) = 32)
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

    CREATE TABLE IF NOT EXISTS __logos_sql_live_suffix (
        position INTEGER PRIMARY KEY AUTOINCREMENT,
        this_msg BLOB NOT NULL UNIQUE CHECK (length(this_msg) = 32),
        tx_id BLOB NOT NULL CHECK (length(tx_id) = 32),
        payload BLOB NOT NULL,
        local INTEGER NOT NULL CHECK (local IN (0, 1))
    ) STRICT;

    CREATE TABLE IF NOT EXISTS __logos_sql_displaced_writes (
        tx_id BLOB PRIMARY KEY CHECK (length(tx_id) = 32),
        this_msg BLOB UNIQUE CHECK (this_msg IS NULL OR length(this_msg) = 32)
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

const INSERT_PENDING_PUBLISH: &str = "
    INSERT INTO __logos_sql_pending_publish (singleton, tx_id, payload)
    VALUES (1, ?1, ?2)
";

const SELECT_PENDING_PUBLISH: &str = "
    SELECT tx_id, payload
    FROM __logos_sql_pending_publish
    WHERE singleton = 1
";

const CLEAR_PENDING_PUBLISH: &str = "
    DELETE FROM __logos_sql_pending_publish
    WHERE singleton = 1 AND tx_id = ?1
";

const INSERT_SUFFIX_WRITE: &str = "
    INSERT INTO __logos_sql_live_suffix (this_msg, tx_id, payload, local)
    VALUES (?1, ?2, ?3, ?4)
    ON CONFLICT (this_msg) DO NOTHING
";

const DELETE_SUFFIX_WRITE: &str = "
    DELETE FROM __logos_sql_live_suffix
    WHERE this_msg = ?1
";

const SELECT_SUFFIX_WRITES: &str = "
    SELECT this_msg, tx_id, payload, local
    FROM __logos_sql_live_suffix
    ORDER BY position
";

const SELECT_LOCAL_SUFFIX_WRITE: &str = "
    SELECT tx_id
    FROM __logos_sql_live_suffix
    WHERE this_msg = ?1 AND local = 1
";

const SELECT_SUFFIX_WRITE_EXISTS: &str = "
    SELECT EXISTS(
        SELECT 1 FROM __logos_sql_live_suffix WHERE this_msg = ?1
    )
";

const INSERT_DISPLACED_WRITE: &str = "
    INSERT INTO __logos_sql_displaced_writes (tx_id, this_msg)
    VALUES (?1, ?2)
    ON CONFLICT (tx_id) DO UPDATE SET this_msg = excluded.this_msg
";

const INSERT_UNPUBLISHED_DISPLACED_WRITE: &str = "
    INSERT INTO __logos_sql_displaced_writes (tx_id, this_msg)
    VALUES (?1, NULL)
    ON CONFLICT (tx_id) DO NOTHING
";

const DELETE_DISPLACED_WRITE: &str = "
    DELETE FROM __logos_sql_displaced_writes
    WHERE this_msg = ?1
";

const SELECT_DISPLACED_WRITES: &str = "
    SELECT tx_id
    FROM __logos_sql_displaced_writes
    ORDER BY rowid
";

const SELECT_DISPLACED_WRITE_EXISTS: &str = "
    SELECT EXISTS(
        SELECT 1 FROM __logos_sql_displaced_writes WHERE tx_id = ?1
    )
";

const SELECT_APPLIED_WRITE: &str = "
    SELECT content_digest
    FROM __logos_sql_applied_writes
    WHERE tx_id = ?1
";

const INSERT_APPLIED_WRITE: &str = "
    INSERT INTO __logos_sql_applied_writes (tx_id, content_digest)
    VALUES (?1, ?2)
";

const WRITER_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = FULL;
";

const REBUILD_PRAGMAS: &str = "
    PRAGMA journal_mode = DELETE;
    PRAGMA synchronous = OFF;
";

const FOREIGN_KEYS_PRAGMA: &str = "PRAGMA foreign_keys = ON;";

// These functions depend on one connection, database file, or SQLite build.
// Their results cannot be reproduced from the ordered channel history.
const UNSUPPORTED_FUNCTIONS: [&str; 9] = [
    "changes",
    "last_insert_rowid",
    "load_extension",
    "sqlite_compileoption_get",
    "sqlite_compileoption_used",
    "sqlite_offset",
    "sqlite_source_id",
    "sqlite_version",
    "total_changes",
];
/// A locally committed write whose `ZoneSDK` checkpoint has not yet been
/// persisted.
///
/// `ZoneSDK` may already hold the write in memory. Until its returned
/// checkpoint and suffix entry commit, this record remains the durable source
/// used to recover the publication.
pub struct PendingPublish {
    pub tx_id: TxId,
    pub payload: Vec<u8>,
}

/// Raw database representation of a pending publication.
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

/// One Logos SQL inscription retained above the finalized boundary.
pub struct SuffixWrite {
    pub this_msg: MsgId,
    pub tx_id: TxId,
    pub payload: Vec<u8>,
    pub local: bool,
}

/// A replicated database connection and the function state attached to it.
struct ReplicatedDatabase {
    connection: Connection,
    functions: FunctionOverrides,
}

/// An isolated replacement for `LIVE.db` while canonical history is replayed.
pub struct LiveRebuild {
    database: ReplicatedDatabase,
    path: PathBuf,
}

impl LiveRebuild {
    pub(crate) fn apply_write(&mut self, write: &ChannelInscription) -> Result<(), Error> {
        apply_channel_write(&mut self.database, write)
    }
}

impl Deref for ReplicatedDatabase {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

/// Owns the participant-local database connections.
pub struct Databases {
    lib: ReplicatedDatabase,
    live: ReplicatedDatabase,
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
        let rebuild_path = directory.join(REBUILD_DATABASE_FILE);

        remove_rebuild_files(&rebuild_path)?;

        let lib = open_writer(&lib_path)?;
        let live = open_writer(&live_path)?;
        let control = open_connection(&control_path)?;

        for connection in [&lib, &live] {
            connection.execute_batch(PENDING_PUBLISH_SCHEMA)?;
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

    /// Persists `ZoneSDK` ownership of a local write and adds it to the live
    /// suffix before removing the pending publication from `LIVE.db`.
    pub(crate) fn complete_publish(
        &mut self,
        checkpoint: &SequencerCheckpoint,
        this_msg: MsgId,
        pending: &PendingPublish,
    ) -> Result<(), Error> {
        let encoded_checkpoint = checkpoint_options().serialize(checkpoint)?;
        let transaction = self.control.transaction()?;

        transaction.execute(UPDATE_CHECKPOINT, [encoded_checkpoint])?;
        transaction.execute(
            INSERT_SUFFIX_WRITE,
            params![
                this_msg.as_ref(),
                pending.tx_id.as_ref(),
                pending.payload,
                true
            ],
        )?;
        transaction.commit()?;

        self.clear_pending_publish(pending.tx_id)
    }

    /// Applies one channel event to retained unfinalized history and local
    /// write outcomes.
    ///
    /// Finalized writes leave the suffix and clear any provisional
    /// displacement. Orphaned local writes become displaced, while restoring
    /// their original channel position clears that outcome again. The whole
    /// delta commits together so replay after a crash cannot observe only one
    /// side of a branch change.
    pub(crate) fn apply_history_delta(
        &mut self,
        finalized: &[MsgId],
        orphaned: &[MsgId],
        adopted: &[SuffixWrite],
    ) -> Result<(), Error> {
        let transaction = self.control.transaction()?;

        for this_msg in finalized {
            transaction.execute(DELETE_DISPLACED_WRITE, [this_msg.as_ref()])?;
            transaction.execute(DELETE_SUFFIX_WRITE, [this_msg.as_ref()])?;
        }

        for this_msg in orphaned {
            let local_tx_id = transaction
                .query_row(SELECT_LOCAL_SUFFIX_WRITE, [this_msg.as_ref()], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .optional()?;

            if let Some(tx_id) = local_tx_id {
                transaction.execute(INSERT_DISPLACED_WRITE, params![tx_id, this_msg.as_ref()])?;
            }

            transaction.execute(DELETE_SUFFIX_WRITE, [this_msg.as_ref()])?;
        }

        for write in adopted {
            let restored_local =
                transaction.execute(DELETE_DISPLACED_WRITE, [write.this_msg.as_ref()])? != 0;

            transaction.execute(
                INSERT_SUFFIX_WRITE,
                params![
                    write.this_msg.as_ref(),
                    write.tx_id.as_ref(),
                    write.payload,
                    write.local || restored_local
                ],
            )?;
        }

        transaction.commit()?;

        Ok(())
    }

    pub(crate) fn live_suffix(&self) -> Result<Vec<SuffixWrite>, Error> {
        let mut statement = self.control.prepare(SELECT_SUFFIX_WRITES)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?;

        rows.map(|row| {
            let (this_msg, tx_id, payload, local) = row?;

            Ok(SuffixWrite {
                this_msg: decode_msg_id(this_msg)?,
                tx_id: decode_tx_id(tx_id)?,
                payload,
                local,
            })
        })
        .collect()
    }

    pub(crate) fn live_suffix_contains(&self, this_msg: MsgId) -> Result<bool, Error> {
        self.control
            .query_row(SELECT_SUFFIX_WRITE_EXISTS, [this_msg.as_ref()], |row| {
                row.get(0)
            })
            .map_err(Error::from)
    }

    pub(crate) fn displaced_writes(&self) -> Result<Vec<TxId>, Error> {
        let mut statement = self.control.prepare(SELECT_DISPLACED_WRITES)?;
        let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;

        rows.map(|row| decode_tx_id(row?)).collect()
    }

    pub(crate) fn is_write_displaced(&self, tx_id: TxId) -> Result<bool, Error> {
        self.control
            .query_row(SELECT_DISPLACED_WRITE_EXISTS, [tx_id.as_ref()], |row| {
                row.get(0)
            })
            .map_err(Error::from)
    }

    /// Records a local write whose base changed before `ZoneSDK` accepted it.
    ///
    /// The pending record remains in `LIVE.db` until the rebuild replaces the
    /// database. If recovery is interrupted, that record makes the same event
    /// request another rebuild.
    pub(crate) fn record_unpublished_displacement(
        &self,
        pending: &PendingPublish,
    ) -> Result<(), Error> {
        self.control
            .execute(INSERT_UNPUBLISHED_DISPLACED_WRITE, [pending.tx_id.as_ref()])?;

        Ok(())
    }

    /// Creates a replacement live database from the finalized image.
    pub(crate) fn begin_live_rebuild(&self) -> Result<LiveRebuild, Error> {
        let path = self
            .live_path
            .parent()
            .ok_or(Error::InvalidLocalState("LIVE.db has no parent directory"))?
            .join(REBUILD_DATABASE_FILE);

        remove_rebuild_files(&path)?;

        let mut database = open_rebuild_writer(&path)?;
        backup_database(&self.lib.connection, &mut database.connection)?;
        database.connection.execute_batch(REBUILD_PRAGMAS)?;
        database.connection.execute_batch(PENDING_PUBLISH_SCHEMA)?;

        Ok(LiveRebuild { database, path })
    }

    /// Atomically replaces the contents of `LIVE.db` for current and future
    /// readers using `SQLite`'s online backup API.
    pub(crate) fn finish_live_rebuild(&mut self, rebuild: LiveRebuild) -> Result<(), Error> {
        backup_database(&rebuild.database.connection, &mut self.live.connection)?;

        let path = rebuild.path.clone();
        drop(rebuild);
        remove_rebuild_files(&path)?;

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
    /// Commits application effects and their pending publication together
    /// in `LIVE.db`.
    pub(crate) fn commit_local_write(
        &mut self,
        tx_id: TxId,
        transaction: &Transaction,
    ) -> Result<TxId, Error> {
        if self.pending_publish()?.is_some() {
            return Err(Error::PublishPending);
        }

        let capture = self.live.functions.capture();
        let db_transaction = self.live.connection.transaction()?;

        apply_statements(&db_transaction, transaction)?;
        let captured_function_calls = capture.finish()?;
        let encoded = EncodedWrite::new(tx_id, transaction, captured_function_calls)?;

        db_transaction.execute(
            INSERT_APPLIED_WRITE,
            params![encoded.tx_id.as_ref(), encoded.content_digest],
        )?;

        db_transaction.execute(
            INSERT_PENDING_PUBLISH,
            params![encoded.tx_id.as_ref(), encoded.payload],
        )?;
        db_transaction.commit()?;

        Ok(encoded.tx_id)
    }

    /// Applies a newly adopted channel write to the live database.
    pub(crate) fn apply_adopted_write(&mut self, write: &ChannelInscription) -> Result<(), Error> {
        apply_channel_write(&mut self.live, write)
    }

    /// Applies a write only to finalized state while a replacement live image
    /// is being built from that state.
    pub(crate) fn apply_finalized_write_to_lib(
        &mut self,
        write: &ChannelInscription,
    ) -> Result<(), Error> {
        apply_channel_write(&mut self.lib, write)
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
        let content_digest = write.content_digest();

        is_write_applied(&self.lib.connection, write.tx_id, &content_digest)?;
        is_write_applied(&self.live.connection, write.tx_id, &content_digest)?;

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
            .connection
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

    pub(crate) fn clear_pending_publish(&self, tx_id: TxId) -> Result<(), Error> {
        let changed = self
            .live
            .connection
            .execute(CLEAR_PENDING_PUBLISH, [tx_id.as_ref()])?;

        if changed != 1 {
            return Err(Error::InvalidLocalState(
                "pending publication record is missing",
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

fn open_writer(path: &Path) -> Result<ReplicatedDatabase, Error> {
    let connection = open_connection(path)?;
    let functions = FunctionOverrides::install(&connection)?;

    Ok(ReplicatedDatabase {
        connection,
        functions,
    })
}

fn open_rebuild_writer(path: &Path) -> Result<ReplicatedDatabase, Error> {
    let connection = Connection::open(path)?;

    configure_connection(&connection)?;
    connection.execute_batch(REBUILD_PRAGMAS)?;

    let functions = FunctionOverrides::install(&connection)?;

    Ok(ReplicatedDatabase {
        connection,
        functions,
    })
}

fn open_connection(path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open(path)?;

    configure_connection(&conn)?;
    conn.execute_batch(WRITER_PRAGMAS)?;

    Ok(conn)
}

fn backup_database(source: &Connection, destination: &mut Connection) -> Result<(), Error> {
    let backup = Backup::new(source, destination)?;
    backup.run_to_completion(BACKUP_PAGES_PER_STEP, BACKUP_RETRY_DELAY, None)?;

    Ok(())
}

fn remove_rebuild_files(path: &Path) -> Result<(), Error> {
    for path in [
        path.to_owned(),
        PathBuf::from(format!("{}-journal", path.display())),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn configure_connection(conn: &Connection) -> Result<(), Error> {
    conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
    conn.execute_batch(FOREIGN_KEYS_PRAGMA)?;

    Ok(())
}

fn apply_channel_write(
    database: &mut ReplicatedDatabase,
    write: &ChannelInscription,
) -> Result<(), Error> {
    let content_digest = write.content_digest();
    let replay = database.functions.replay(&write.captured_function_calls);
    let db_transaction = database.connection.transaction()?;

    if is_write_applied(&db_transaction, write.tx_id, &content_digest)? {
        return Ok(());
    }

    if let Err(error) = apply_statements(&db_transaction, &write.transaction) {
        if replay.failed() {
            return Err(Error::InvalidPayload(
                "captured SQLite function call does not match replay",
            ));
        }

        return match error {
            Error::Database(error) if is_deterministic_sql_error(&error) => {
                Err(Error::RejectedSql(error))
            }
            error => Err(error),
        };
    }

    replay.finish()?;

    db_transaction.execute(
        INSERT_APPLIED_WRITE,
        params![write.tx_id.as_ref(), content_digest],
    )?;
    db_transaction.commit()?;

    Ok(())
}

fn is_write_applied(
    connection: &Connection,
    tx_id: TxId,
    content_digest: &[u8; 32],
) -> Result<bool, Error> {
    let stored_digest = connection
        .query_row(SELECT_APPLIED_WRITE, [tx_id.as_ref()], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()?;

    if let Some(stored_digest) = stored_digest {
        if stored_digest.as_slice() != content_digest {
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
        || action_uses_reserved_name(context.action)
        || matches!(
            context.action,
            AuthAction::Function { function_name }
                if UNSUPPORTED_FUNCTIONS
                    .iter()
                    .any(|name| function_name.eq_ignore_ascii_case(name))
        );

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

fn decode_msg_id(bytes: Vec<u8>) -> Result<MsgId, Error> {
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::InvalidLocalState("stored message id is malformed"))?;

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
    use rusqlite::{Connection, types::Value};
    use tempfile::TempDir;

    use super::Databases;
    use crate::{
        error::Error,
        protocol::{
            CapturedFunction, CapturedFunctionCall, CapturedFunctionCalls, ChannelInscription,
            Statement, Transaction, TxId,
        },
    };

    fn checkpoint(byte: u8, slot: u64) -> SequencerCheckpoint {
        SequencerCheckpoint {
            last_msg_id: MsgId::root(),
            pending_txs: Vec::new(),
            lib: HeaderId::from([byte; 32]),
            lib_slot: Slot::from(slot),
            channel_notes: Vec::new(),
            finalized_config: MsgId::root(),
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

    fn row_values(connection: &Connection, table: &str) -> Vec<Value> {
        connection
            .query_row(&format!("SELECT * FROM {table}"), [], |row| {
                (0..row.as_ref().column_count())
                    .map(|column| row.get(column))
                    .collect()
            })
            .expect("captured row should be readable")
    }

    fn rejected_application_sql(sql: &str) -> Error {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value INTEGER)", [])
            .expect("application table should be created");

        let transaction = transaction(sql);
        let error = db
            .commit_local_write(TxId::generate(), &transaction)
            .expect_err("application SQL should be rejected");

        assert!(
            db.pending_publish()
                .expect("pending publication should load")
                .is_none()
        );

        error
    }

    fn assert_application_sql_rejected(sql: &str) {
        assert!(matches!(rejected_application_sql(sql), Error::Database(_)));
    }

    fn assert_application_sql_denied(sql: &str) {
        let error = rejected_application_sql(sql);

        assert!(matches!(
            error,
            Error::Database(ref error)
                if error.sqlite_error_code()
                    == Some(rusqlite::ErrorCode::AuthorizationForStatementDenied)
        ));
    }

    fn internal_schema(connection: &Connection) -> Vec<(String, String, String, Option<String>)> {
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
    fn finalization_clears_a_provisional_displacement() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let tx_id = db
            .commit_local_write(
                TxId::generate(),
                &transaction("CREATE TABLE local_write(value INTEGER)"),
            )
            .expect("local write should commit");
        let pending = db
            .pending_publish()
            .expect("pending write should load")
            .expect("pending write should exist");
        let this_msg = MsgId::from([7; 32]);

        db.complete_publish(&checkpoint(1, 1), this_msg, &pending)
            .expect("publish should be complete");
        db.apply_history_delta(&[], &[this_msg], &[])
            .expect("local write should be orphaned");

        assert_eq!(
            db.displaced_writes().expect("displaced writes should load"),
            vec![tx_id]
        );

        db.apply_history_delta(&[this_msg], &[], &[])
            .expect("finalized suffix should be removed");

        assert!(
            db.displaced_writes()
                .expect("displaced writes should load")
                .is_empty()
        );
    }

    #[test]
    fn application_write_and_pending_publish_commit_together() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value TEXT NOT NULL)", [])
            .expect("application table should be created");

        let transaction = insert("hello");
        let tx_id = db
            .commit_local_write(TxId::generate(), &transaction)
            .expect("write should commit");

        let count: i64 = db
            .live
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("row should be readable");

        assert_eq!(count, 1);
        assert_eq!(
            db.pending_publish()
                .expect("pending publication should load")
                .expect("pending publication should exist")
                .tx_id,
            tx_id
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
        let first_tx_id = db
            .commit_local_write(TxId::generate(), &transaction)
            .expect("first write should commit");
        db.clear_pending_publish(first_tx_id)
            .expect("first publication should complete");

        let second_tx_id = db
            .commit_local_write(TxId::generate(), &transaction)
            .expect("second write should commit");

        assert_ne!(second_tx_id, first_tx_id);

        let count: i64 = db
            .live
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .expect("row should be readable");

        assert_eq!(count, 2);
    }

    #[test]
    fn function_results_are_replayed_exactly() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let schema = "CREATE TABLE captured(
            random_value,
            random_blob_value,
            date_value,
            time_value,
            datetime_value,
            julian_day_value,
            unix_epoch_value,
            strftime_value,
            time_diff_value,
            current_date_value,
            current_time_value,
            current_timestamp_value
        )";

        for connection in [&db.lib, &db.live] {
            connection
                .execute(schema, [])
                .expect("application table should be created");
        }

        let write = transaction(
            "INSERT INTO captured VALUES (
                random(),
                randomblob(16),
                date('now'),
                time('now'),
                datetime('now'),
                julianday('now'),
                unixepoch('now'),
                strftime('%s', 'now'),
                timediff('now', 'now'),
                CURRENT_DATE,
                CURRENT_TIME,
                CURRENT_TIMESTAMP
            )",
        );
        db.commit_local_write(TxId::generate(), &write)
            .expect("local write should commit");

        let pending = db
            .pending_publish()
            .expect("pending publication should load")
            .expect("pending publication should exist");
        let channel_inscription =
            ChannelInscription::decode(&pending.payload).expect("payload should decode");
        let functions = channel_inscription
            .captured_function_calls
            .as_slice()
            .iter()
            .map(|call| call.function)
            .collect::<Vec<_>>();

        assert_eq!(
            functions,
            vec![
                CapturedFunction::Random,
                CapturedFunction::RandomBlob,
                CapturedFunction::Date,
                CapturedFunction::Time,
                CapturedFunction::DateTime,
                CapturedFunction::JulianDay,
                CapturedFunction::UnixEpoch,
                CapturedFunction::Strftime,
                CapturedFunction::TimeDiff,
                CapturedFunction::CurrentDate,
                CapturedFunction::CurrentTime,
                CapturedFunction::CurrentTimestamp,
            ]
        );

        db.apply_finalized_write(&channel_inscription)
            .expect("captured write should replay");

        assert_eq!(
            row_values(&db.live, "captured"),
            row_values(&db.lib, "captured")
        );
    }

    #[test]
    fn function_calls_inside_defaults_and_triggers_are_captured() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let schema = "
            CREATE TABLE items(
                value TEXT,
                created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE audit(random_value INTEGER);
            CREATE TRIGGER audit_insert AFTER INSERT ON items BEGIN
                INSERT INTO audit VALUES (random());
            END;
        ";

        for connection in [&db.lib, &db.live] {
            connection
                .execute_batch(schema)
                .expect("application schema should be created");
        }

        let write = transaction("INSERT INTO items(value) VALUES ('hello')");
        db.commit_local_write(TxId::generate(), &write)
            .expect("local write should commit");

        let pending = db
            .pending_publish()
            .expect("pending publication should load")
            .expect("pending publication should exist");
        let channel_inscription =
            ChannelInscription::decode(&pending.payload).expect("payload should decode");

        assert_eq!(
            channel_inscription.captured_function_calls.as_slice().len(),
            2
        );

        db.apply_finalized_write(&channel_inscription)
            .expect("trigger write should replay");

        assert_eq!(row_values(&db.live, "items"), row_values(&db.lib, "items"));
        assert_eq!(row_values(&db.live, "audit"), row_values(&db.lib, "audit"));
    }

    #[test]
    fn missing_function_result_rejects_channel_inscription() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value INTEGER)", [])
            .expect("application table should be created");

        let write = ChannelInscription {
            tx_id: TxId::from([7; 32]),
            transaction: transaction("INSERT INTO items VALUES (random())"),
            captured_function_calls: CapturedFunctionCalls::empty(),
        };

        let error = db
            .apply_adopted_write(&write)
            .expect_err("missing result should reject the write");

        assert!(matches!(error, Error::InvalidPayload(_)));
        assert_eq!(
            db.live
                .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
                .expect("row count should be readable"),
            0
        );
    }

    #[test]
    fn unused_function_result_rejects_channel_inscription() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");

        db.live
            .execute("CREATE TABLE items(value INTEGER)", [])
            .expect("application table should be created");

        let captured = CapturedFunctionCall::new(CapturedFunction::Random, Value::Integer(7))
            .expect("captured result should be valid");
        let write = ChannelInscription {
            tx_id: TxId::from([7; 32]),
            transaction: transaction("INSERT INTO items VALUES (1)"),
            captured_function_calls: CapturedFunctionCalls::new(vec![captured])
                .expect("captured calls should be valid"),
        };

        let error = db
            .apply_adopted_write(&write)
            .expect_err("unused result should reject the write");

        assert!(matches!(error, Error::InvalidPayload(_)));
        assert_eq!(
            db.live
                .query_row("SELECT count(*) FROM items", [], |row| row.get::<_, i64>(0))
                .expect("row count should be readable"),
            0
        );
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
            captured_function_calls: CapturedFunctionCalls::empty(),
        };
        let conflicting = ChannelInscription {
            tx_id,
            transaction: insert("conflicting"),
            captured_function_calls: CapturedFunctionCalls::empty(),
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
            captured_function_calls: CapturedFunctionCalls::empty(),
        };
        let conflicting = ChannelInscription {
            tx_id,
            transaction: insert("conflicting"),
            captured_function_calls: CapturedFunctionCalls::empty(),
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
        db.commit_local_write(TxId::generate(), &transaction)
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
            db.commit_local_write(TxId::generate(), &transaction)
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
            assert_application_sql_denied(sql);
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
            assert_application_sql_denied(sql);
        }
    }

    #[test]
    fn connection_dependent_functions_are_rejected() {
        for function in [
            "changes()",
            "last_insert_rowid()",
            "sqlite_version()",
            "total_changes()",
        ] {
            assert_application_sql_rejected(&format!("SELECT {function}"));
        }
    }

    #[test]
    fn application_sql_cannot_create_temporary_objects() {
        for sql in [
            "CREATE TEMP TABLE temporary_items(value INTEGER)",
            "CREATE TEMP VIEW temporary_items AS SELECT 1",
        ] {
            assert_application_sql_denied(sql);
        }
    }

    #[test]
    fn reserved_name_check_accepts_non_ascii_identifiers() {
        let dir = TempDir::new().expect("temporary directory should be created");
        let mut db = Databases::open(dir.path()).expect("databases should open");
        let transaction = transaction("CREATE TABLE aaaaaaaaaa\u{65e5}(value INTEGER)");

        db.commit_local_write(TxId::generate(), &transaction)
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
