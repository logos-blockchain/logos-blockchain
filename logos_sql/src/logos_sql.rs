//! Application entry point: a running `λSQL` participant.

use std::path::PathBuf;

use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_zone_sdk::{
    CommonHttpClient,
    adapter::NodeHttpClient,
    node_types::ChannelId,
    sequencer::{FundingConfig, ZoneSequencer},
};
use rand::rngs::OsRng;
use reqwest::Url;
use rusqlite::Connection;

use crate::{
    db::Databases,
    error::Error,
    protocol::TxId,
    runtime,
    sql::TransactionBuilder,
    status::{Displacement, WriteStatus},
};

/// Configuration for one `λSQL` database.
pub struct LogosSqlConfig {
    /// Channel carrying the write log.
    pub channel_id: ChannelId,
    /// Base URL of the node HTTP API.
    pub node_url: Url,
    /// Directory containing this participant's local databases.
    pub state_dir: PathBuf,
    /// Signing and funding configuration. `None` follows the channel read-only.
    pub writer: Option<WriterConfig>,
}

/// Credentials needed to publish writes to the channel.
pub struct WriterConfig {
    /// Key used to sign published inscriptions.
    pub signing_key: Ed25519Key,
    /// Fee funding for published transactions.
    pub funding: FundingConfig,
}

/// A running `λSQL` database.
///
/// `LogosSql` owns one background task. That task is the only owner of both the
/// `ZoneSDK` sequencer and the database writer. Dropping `LogosSql` aborts the
/// task; call [`Self::shutdown`] to stop it gracefully and observe errors.
pub struct LogosSql {
    lib_path: PathBuf,
    live_path: PathBuf,
    runtime: Option<runtime::RuntimeHandle>,
}

impl LogosSql {
    /// Opens local state, starts replication, and waits for the initial channel
    /// history to be processed.
    ///
    /// With `writer: None`, this follows the channel without publishing.
    /// Execution and displacement retries then return [`Error::ReadOnly`].
    ///
    /// Must be called from within a tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if no Tokio runtime is active, the local state cannot
    /// be opened, or the replication task stops before becoming ready.
    /// Read-only startup also rejects state with pending publications.
    pub async fn start(config: LogosSqlConfig) -> Result<Self, Error> {
        tokio::runtime::Handle::try_current().map_err(|_| Error::RuntimeUnavailable)?;

        let db = Databases::open(&config.state_dir)?;
        let lib_path = db.lib_path().to_owned();
        let live_path = db.live_path().to_owned();
        let checkpoint = db.load_checkpoint()?;
        let node = NodeHttpClient::new(CommonHttpClient::new(None), config.node_url);

        let read_only = config.writer.is_none();
        let writer = if let Some(writer) = config.writer {
            writer
        } else {
            if db.pending_publish()?.is_some()
                || checkpoint
                    .as_ref()
                    .is_some_and(|cp| !cp.pending_txs.is_empty())
            {
                return Err(Error::InvalidLocalState(
                    "read-only startup cannot resume pending publications",
                ));
            }

            // ZoneSDK's observer setup still requires a key and funding config.
            // Use a fresh key and inert funding; the runtime blocks publication.
            WriterConfig {
                signing_key: Ed25519Key::generate(&mut OsRng),
                funding: FundingConfig {
                    funding_pk: ZkPublicKey::zero(),
                    change_pk: None,
                    max_tx_fee: 0u64.into(),
                    priority_fee_percent: 0,
                },
            }
        };

        let sequencer = ZoneSequencer::init(
            config.channel_id,
            writer.signing_key,
            node,
            writer.funding,
            checkpoint.clone(),
        );

        let runtime = runtime::spawn(sequencer, db, config.channel_id, checkpoint, read_only);

        let mut logos_sql = Self {
            lib_path,
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

    /// Executes a prepared SQL transaction locally and submits it for
    /// publication.
    ///
    /// ```no_run
    /// # use logos_sql::{Error, LogosSql, TxId, TransactionBuilder};
    /// # async fn create_task(logos_sql: &LogosSql) -> Result<TxId, Error> {
    /// let transaction = TransactionBuilder::new(
    ///     "INSERT INTO tasks (id, title) VALUES (?1, ?2)",
    /// )
    ///     .bind(42i64)
    ///     .bind("Write documentation");
    /// let tx_id = transaction.tx_id();
    ///
    /// assert_eq!(logos_sql.execute(transaction).await?, tx_id);
    /// Ok(tx_id)
    /// # }
    /// ```
    ///
    /// A successful return means the SQL effects and recovery record are
    /// committed locally. Publication and finality remain asynchronous.
    /// [`TransactionBuilder::tx_id`] exposes the same identity before the
    /// asynchronous call, allowing the application to record how transaction
    /// outcomes map to its own operations.
    ///
    /// # Errors
    ///
    /// Returns an error when a parameter cannot be represented by the `λSQL`
    /// protocol, validation or the local commit fails, the sequencer is not
    /// ready, or the runtime has halted. Returns
    /// [`Error::UnhandledDisplacements`] without executing SQL while local
    /// displacements await application handling.
    ///
    /// Coordinate conflict handling with all application writers: after
    /// handling displacements, reconsider work prepared from the old state.
    /// This is a participant-wide gate, not a per-transaction freshness check.
    pub async fn execute(&self, transaction: TransactionBuilder) -> Result<TxId, Error> {
        let (tx_id, transaction) = transaction.finish()?;

        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .execute(tx_id, transaction)
            .await
    }

    /// Resubmits a displaced write's original SQL and parameters.
    ///
    /// Unlike [`Self::execute`], this deliberately allows execution while
    /// displacements await handling. It creates a fresh `TxId` and evaluates
    /// time and random functions again. All other execution checks still apply.
    ///
    /// On success, the displacement is marked handled.
    /// If the call fails or is interrupted, the retry may already have
    /// committed.
    ///
    /// The original write can still return after a reorganization. Only retry
    /// SQL designed to tolerate that; this does not deduplicate the two writes.
    ///
    /// # Errors
    /// Returns execution errors as [`Self::execute`] does, except that
    /// unhandled displacements do not block this call. Also returns an error
    /// if marking the displacement handled fails.
    /// Returns [`Error::StaleDisplacement`] without executing SQL if this
    /// displacement is no longer awaiting handling.
    pub async fn retry_displacement(&self, displacement: &Displacement) -> Result<TxId, Error> {
        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .retry_displacement(displacement.clone())
            .await
    }

    /// Returns the current status of a local write.
    ///
    /// `None` means this participant has no record of `tx_id`. A displaced
    /// write can become live again if a channel reorganization restores it.
    /// Once a write is [`WriteStatus::Finalized`], its status cannot change.
    ///
    /// # Errors
    ///
    /// Returns an error if the runtime has stopped or local status cannot be
    /// read.
    pub async fn write_status(&self, tx_id: TxId) -> Result<Option<WriteStatus>, Error> {
        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .write_status(tx_id)
            .await
    }

    /// Lists local displacements that still need an application decision.
    ///
    /// Reading does not mark them handled. A displacement is cleared when its
    /// write returns to live history or finalizes. If it is displaced again,
    /// it appears as a new displacement.
    /// Each displacement retains its original SQL and parameters for
    /// [`Self::retry_displacement`].
    ///
    /// # Errors
    /// Returns an error if the runtime is stopped, halted, or local state
    /// cannot be read.
    pub async fn unhandled_displacements(&self) -> Result<Vec<Displacement>, Error> {
        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .unhandled_displacements()
            .await
    }

    /// Records that the application has considered this displacement.
    ///
    /// This does not discard, republish, or change the chain status of the
    /// write. Handling an older displacement cannot clear a newer one.
    /// Repeated calls are harmless. Execution stays blocked while any
    /// displacement is unhandled.
    ///
    /// # Errors
    /// Returns an error if the runtime is stopped, halted, or local state
    /// cannot be saved.
    pub async fn mark_displacement_handled(&self, displacement: Displacement) -> Result<(), Error> {
        self.runtime
            .as_ref()
            .ok_or(Error::RuntimeStopped)?
            .mark_displacement_handled(displacement)
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

    /// Opens a read-only connection to finalized state.
    ///
    /// Unlike [`Self::read_connection`], this state cannot be displaced by a
    /// channel reorganization.
    ///
    /// # Errors
    ///
    /// Returns an error when the database file cannot be opened.
    pub fn finalized_read_connection(&self) -> Result<Connection, Error> {
        Databases::open_reader(&self.lib_path)
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::{LogosSql, LogosSqlConfig};
    use crate::{db::Databases, error::Error, sql::TransactionBuilder};

    #[tokio::test]
    async fn read_only_start_does_not_abandon_a_pending_write() {
        let directory = TempDir::new().unwrap();
        let mut db = Databases::open(directory.path()).unwrap();
        let (tx_id, transaction) = TransactionBuilder::new("CREATE TABLE items(value INTEGER)")
            .finish()
            .unwrap();
        db.commit_local_write(tx_id, &transaction).unwrap();
        drop(db);

        let result = LogosSql::start(LogosSqlConfig {
            channel_id: [1; 32].into(),
            writer: None,
            node_url: "http://127.0.0.1:1".parse().unwrap(),
            state_dir: directory.path().to_owned(),
        })
        .await;

        assert!(matches!(result, Err(Error::InvalidLocalState(_))));

        let db = Databases::open(directory.path()).unwrap();
        assert_eq!(db.pending_publish().unwrap().unwrap().tx_id, tx_id);
    }
}
