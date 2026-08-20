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
use rusqlite::Connection;

use crate::{
    db::Databases,
    error::Error,
    protocol::{IdempotencyKey, Transaction, TxId},
    runtime,
    sql::{QueryBuilder, TransactionBuilder},
};

/// Configuration for one `λSQL` database.
pub struct LogosSqlConfig {
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
/// `LogosSql` owns one background task. That task is the only owner of both the
/// `ZoneSDK` sequencer and the database writer. Dropping `LogosSql` aborts the
/// task; call [`Self::shutdown`] to stop it gracefully and observe errors.
pub struct LogosSql {
    live_path: PathBuf,
    runtime: Option<runtime::RuntimeHandle>,
}

impl LogosSql {
    /// Opens local state, starts replication, and waits for the initial channel
    /// history to be processed.
    ///
    /// Must be called from within a tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if no Tokio runtime is active, the local state cannot
    /// be opened, or the replication task stops before becoming ready.
    pub async fn start(config: LogosSqlConfig) -> Result<Self, Error> {
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

        let mut logos_sql = Self {
            live_path,
            runtime: Some(runtime),
        };

        logos_sql
            .runtime
            .as_mut()
            .ok_or(Error::RuntimeStopped)?
            .wait_until_ready()
            .await?;

        Ok(logos_sql)
    }

    /// Starts a replicated write containing one SQL statement.
    ///
    /// Bind parameters with [`QueryBuilder::bind`], then call
    /// [`QueryBuilder::execute`] to execute the statement locally and
    /// submit it for publication.
    ///
    /// ```no_run
    /// # use logos_sql::{Error, IdempotencyKey, LogosSql, TxId};
    /// # async fn create_task(
    /// #     logos_sql: &LogosSql,
    /// #     key: IdempotencyKey,
    /// # ) -> Result<TxId, Error> {
    /// logos_sql
    ///     .query("INSERT INTO tasks (id, title) VALUES (?1, ?2)")
    ///     .bind(42i64)
    ///     .bind("Write documentation")
    ///     .execute(key)
    ///     .await
    /// # }
    /// ```
    pub fn query(&self, sql: impl Into<String>) -> QueryBuilder<'_> {
        self.transaction().query(sql)
    }

    /// Starts an atomic replicated write containing multiple SQL statements.
    pub const fn transaction(&self) -> TransactionBuilder<'_> {
        TransactionBuilder::new(self)
    }

    pub(crate) async fn commit_transaction(
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

impl Drop for LogosSql {
    fn drop(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.abort();
        }
    }
}
