//! Application entry point: a running `λSQL` participant.

use std::path::PathBuf;

use lb_key_management_system_service::keys::Ed25519Key;
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    node_types::ChannelId,
    sequencer::{FundingConfig, ZoneSequencer},
};
use reqwest::Url;
use rusqlite::{
    Connection,
    types::{ToSql, ToSqlOutput, Value, ValueRef},
};

use crate::{
    db::Databases,
    error::Error,
    protocol::{IdempotencyKey, Statement, Transaction, TxId},
    runtime,
};

/// Configuration for one `λSQL` database.
pub struct LsqlConfig {
    /// Channel carrying the write log.
    pub channel_id: ChannelId,
    /// Key used to sign published inscriptions.
    pub signing_key: Ed25519Key,
    /// Base URL of the node HTTP API.
    pub node_url: Url,
    /// Fee funding for published transactions.
    pub funding: FundingConfig,
    /// Directory containing this participant's local databases.
    pub state_dir: PathBuf,
}

/// A running `λSQL` database.
///
/// `Lsql` owns one background task. That task is the only owner of both the
/// `ZoneSDK` sequencer and the database writer. Dropping `Lsql` aborts the
/// task; call [`Self::shutdown`] to stop it gracefully and observe errors.
pub struct Lsql {
    live_path: PathBuf,
    runtime: Option<runtime::RuntimeHandle>,
}

impl Lsql {
    /// Opens local state and starts the task that connects to the node and
    /// drives replication.
    ///
    /// Must be called from within a tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if no Tokio runtime is active or the local state cannot
    /// be opened.
    pub fn start(config: LsqlConfig) -> Result<Self, Error> {
        tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;

        let db = Databases::open(&config.state_dir)?;
        let live_path = db.live_path().to_owned();
        let checkpoint = db.load_checkpoint()?;
        let writer_id = config.signing_key.public_key().to_bytes();

        let node = NodeHttpClient::new(CommonHttpClient::new(None), config.node_url);

        let sequencer = ZoneSequencer::init(
            config.channel_id,
            config.signing_key,
            node,
            config.funding,
            checkpoint.clone(),
        );

        let runtime = runtime::spawn(sequencer, db, config.channel_id, writer_id, checkpoint);

        Ok(Self {
            live_path,
            runtime: Some(runtime),
        })
    }

    /// Starts a replicated write containing one SQL statement.
    ///
    /// Bind parameters with [`TransactionBuilder::bind`], then call
    /// [`TransactionBuilder::execute`] to execute the statement locally and
    /// submit it for publication.
    ///
    /// ```no_run
    /// # use logos_blockchain_lsql::{Error, IdempotencyKey, Lsql, TxId};
    /// # async fn create_task(
    /// #     lsql: &Lsql,
    /// #     key: IdempotencyKey,
    /// # ) -> Result<TxId, Error> {
    /// lsql.query("INSERT INTO tasks (id, title) VALUES (?1, ?2)")
    ///     .bind(42i64)
    ///     .bind("Write documentation")
    ///     .execute(key)
    ///     .await
    /// # }
    /// ```
    pub fn query(&self, sql: impl Into<String>) -> TransactionBuilder<'_> {
        self.transaction().query(sql)
    }

    /// Starts an atomic replicated write containing multiple SQL statements.
    pub const fn transaction(&self) -> TransactionBuilder<'_> {
        TransactionBuilder::new(self)
    }

    async fn commit_transaction(
        &self,
        transaction: Transaction,
        idempotency_key: IdempotencyKey,
    ) -> Result<TxId, Error> {
        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .execute(transaction, idempotency_key)
            .await
    }

    /// Opens a read-only connection to the replicated database.
    ///
    /// # Errors
    ///
    /// Returns an error when the database file cannot be opened.
    pub fn read_connection(&self) -> Result<Connection, Error> {
        Databases::open_reader(&self.live_path)
    }

    /// Stops the runtime after its current atomic operation and waits for it.
    ///
    /// # Errors
    ///
    /// Returns the runtime error if the task had already failed, or a join
    /// error if the task was cancelled or panicked.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown().await?;
        }

        Ok(())
    }
}

/// A replicated SQL transaction being assembled by the application.
///
/// Create one with [`Lsql::query`] for a single statement or
/// [`Lsql::transaction`] for multiple statements. Parameter values are
/// converted immediately into owned `SQLite` values, so borrowed application
/// data does not need to outlive the call to [`Self::bind`].
#[must_use = "the transaction must be executed to apply its SQL"]
pub struct TransactionBuilder<'a> {
    lsql: &'a Lsql,
    statements: Vec<Statement>,
    current: Option<PendingStatement>,
    error: Option<Error>,
}

impl<'a> TransactionBuilder<'a> {
    const fn new(lsql: &'a Lsql) -> Self {
        Self {
            lsql,
            statements: Vec::new(),
            current: None,
            error: None,
        }
    }

    /// Adds the next SQL query to this transaction.
    pub fn query(mut self, sql: impl Into<String>) -> Self {
        self.finish_current_statement();

        if self.error.is_none() {
            self.current = Some(PendingStatement {
                sql: sql.into(),
                params: Vec::new(),
            });
        }

        self
    }

    /// Binds one parameter to the current statement.
    ///
    /// Values use [`rusqlite::types::ToSql`], the same conversion interface as
    /// ordinary `SQLite` writes. Any conversion error is returned by
    /// [`Self::execute`].
    pub fn bind<T>(mut self, value: T) -> Self
    where
        T: ToSql,
    {
        if self.error.is_some() {
            return self;
        }

        let Some(statement) = &mut self.current else {
            self.error = Some(Error::InvalidTransaction(
                "a statement must be added before binding parameters",
            ));
            return self;
        };

        match owned_sql_value(&value) {
            Ok(value) => statement.params.push(value),
            Err(error) => self.error = Some(error),
        }

        self
    }

    /// Commits the transaction locally and submits it to `ZoneSDK`.
    ///
    /// A successful return means the SQL effects and recovery record are
    /// committed locally. Publication and finality remain asynchronous.
    ///
    /// # Errors
    ///
    /// Returns an error when a parameter cannot be represented by the `λSQL`
    /// protocol, validation or the local commit fails, the sequencer is not
    /// ready, or the runtime has halted.
    pub async fn execute(mut self, idempotency_key: IdempotencyKey) -> Result<TxId, Error> {
        self.finish_current_statement();

        if let Some(error) = self.error {
            return Err(error);
        }

        let transaction = Transaction::new(self.statements)?;

        self.lsql
            .commit_transaction(transaction, idempotency_key)
            .await
    }

    fn finish_current_statement(&mut self) {
        if self.error.is_some() {
            return;
        }

        let Some(statement) = self.current.take() else {
            return;
        };

        match Statement::new(statement.sql, statement.params) {
            Ok(statement) => self.statements.push(statement),
            Err(error) => self.error = Some(error),
        }
    }
}

struct PendingStatement {
    sql: String,
    params: Vec<Value>,
}

fn owned_sql_value(value: &impl ToSql) -> Result<Value, Error> {
    match value.to_sql()? {
        ToSqlOutput::Borrowed(value) => owned_value_ref(value),
        ToSqlOutput::Owned(value) => Ok(value),
        _ => Err(Error::UnsupportedParameter),
    }
}

fn owned_value_ref(value: ValueRef<'_>) -> Result<Value, Error> {
    match value {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(value) => Ok(Value::Integer(value)),
        ValueRef::Real(value) => Ok(Value::Real(value)),
        ValueRef::Text(value) => String::from_utf8(value.to_vec())
            .map(Value::Text)
            .map_err(|_| Error::InvalidParameter("text parameter is not valid UTF-8")),
        ValueRef::Blob(value) => Ok(Value::Blob(value.to_vec())),
    }
}

impl Drop for Lsql {
    fn drop(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::types::Value;

    use super::owned_sql_value;

    #[test]
    fn converts_sql_parameters_to_owned_values() {
        let text = String::from("hello");

        assert_eq!(owned_sql_value(&42i64).unwrap(), Value::Integer(42));
        assert_eq!(owned_sql_value(&text.as_str()).unwrap(), Value::Text(text));
    }

    #[test]
    fn converts_none_to_sql_null() {
        assert_eq!(owned_sql_value(&Option::<i64>::None).unwrap(), Value::Null);
    }
}
