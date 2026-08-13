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

    /// Executes and durably records one local SQL transaction before submitting
    /// it to `ZoneSDK`.
    ///
    /// A successful return means the SQL effects and recovery record are
    /// committed locally. Publication and finality remain asynchronous.
    ///
    /// # Errors
    ///
    /// Returns an error when validation or the local commit fails, the
    /// sequencer is not ready, or the runtime has halted.
    pub async fn execute(
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

impl Drop for Lsql {
    fn drop(&mut self) {
        if let Some(runtime) = &self.runtime {
            runtime.abort();
        }
    }
}
