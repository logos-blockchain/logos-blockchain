//! Single-owner runtime for SQL writes and channel events.

use std::{num::NonZeroU16, time::Duration};

use lb_zone_sdk::{
    adapter::NodeHttpClient,
    node_types::{ChannelId, Inscription},
    sequencer::{Event, SequencerCheckpoint, ZoneSequencer, channel_inscriptions},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior, interval, sleep_until},
};

use crate::{
    applier,
    db::Databases,
    error::Error,
    protocol::{Transaction, TxId},
    publication::Publication,
    status::{Displacement, WriteStatus},
};

const COMMAND_CHANNEL_CAPACITY: usize = 16;
const PUBLISH_RETRY_INTERVAL: Duration = Duration::from_secs(5);
// A short window lets sequential execute calls share publication overhead.
const BATCH_DELAY: Duration = Duration::from_millis(10);
const TARGET: &str = lb_log_targets::logos_sql::RUNTIME;

/// Requests processed by the task that owns the sequencer and database writer.
enum Command {
    Execute {
        tx_id: TxId,
        transaction: Transaction,
        response_tx: oneshot::Sender<Result<TxId, Error>>,
    },
    RetryDisplacement {
        displacement: Displacement,
        response_tx: oneshot::Sender<Result<TxId, Error>>,
    },
    HandleDisplacement {
        displacement: Displacement,
        response_tx: oneshot::Sender<Result<(), Error>>,
    },
    WriteStatus {
        tx_id: TxId,
        response_tx: oneshot::Sender<Result<Option<WriteStatus>, Error>>,
    },
    UnhandledDisplacements {
        response_tx: oneshot::Sender<Result<Vec<Displacement>, Error>>,
    },
    Shutdown,
}

/// Control surface for the owning runtime task.
pub struct RuntimeHandle {
    command_tx: mpsc::Sender<Command>,
    ready_rx: oneshot::Receiver<()>,
    task: JoinHandle<Result<(), Error>>,
}

/// Starts the task that owns the sequencer and writable database connections.
pub fn spawn(
    sequencer: ZoneSequencer<NodeHttpClient>,
    db: Databases,
    channel_id: ChannelId,
    restored_checkpoint: Option<SequencerCheckpoint>,
    read_only: bool,
    max_batch_transactions: NonZeroU16,
) -> RuntimeHandle {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (ready_tx, ready_rx) = oneshot::channel();

    let runtime = Runtime {
        db,
        channel_id,
        read_only,
        max_batch_transactions,
        command_rx,
        sequencer_ready: false,
        ready_checkpoint_pending: false,
        ready_tx: Some(ready_tx),
        event_pending_retry: None,
        publish_state: PublishState::Idle,
        next_publish_at: Instant::now() + BATCH_DELAY,
    };
    let task = tokio::spawn(runtime.run(sequencer, restored_checkpoint));

    RuntimeHandle {
        command_tx,
        ready_rx,
        task,
    }
}

impl RuntimeHandle {
    pub(crate) async fn wait_until_ready(&mut self) -> Result<(), Error> {
        tokio::select! {
            biased;

            result = &mut self.task => {
                match result? {
                    Ok(()) => Err(Error::RuntimeStopped),
                    Err(error) => Err(error),
                }
            }
            result = &mut self.ready_rx => {
                result.map_err(|_| Error::RuntimeStopped)
            }
        }
    }

    pub(crate) async fn execute(
        &self,
        tx_id: TxId,
        transaction: Transaction,
    ) -> Result<TxId, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::Execute {
                tx_id,
                transaction,
                response_tx,
            })
            .await
            .map_err(|_| Error::RuntimeStopped)?;

        response_rx.await.map_err(|_| Error::RuntimeStopped)?
    }

    pub(crate) async fn retry_displacement(
        &self,
        displacement: Displacement,
    ) -> Result<TxId, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::RetryDisplacement {
                displacement,
                response_tx,
            })
            .await
            .map_err(|_| Error::RuntimeStopped)?;

        response_rx.await.map_err(|_| Error::RuntimeStopped)?
    }

    pub(crate) async fn write_status(&self, tx_id: TxId) -> Result<Option<WriteStatus>, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::WriteStatus { tx_id, response_tx })
            .await
            .map_err(|_| Error::RuntimeStopped)?;

        response_rx.await.map_err(|_| Error::RuntimeStopped)?
    }

    pub(crate) async fn mark_displacement_handled(
        &self,
        displacement: Displacement,
    ) -> Result<(), Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::HandleDisplacement {
                displacement,
                response_tx,
            })
            .await
            .map_err(|_| Error::RuntimeStopped)?;

        response_rx.await.map_err(|_| Error::RuntimeStopped)?
    }

    pub(crate) async fn unhandled_displacements(&self) -> Result<Vec<Displacement>, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::UnhandledDisplacements { response_tx })
            .await
            .map_err(|_| Error::RuntimeStopped)?;

        response_rx.await.map_err(|_| Error::RuntimeStopped)?
    }

    pub(crate) async fn shutdown(self) -> Result<(), Error> {
        drop(self.command_tx.send(Command::Shutdown).await);

        self.task.await?
    }

    pub(crate) fn abort(&self) {
        self.task.abort();
    }
}

/// A `ZoneSDK` publish whose returned checkpoint may still need to be
/// persisted.
enum PublishState {
    Idle,
    CheckpointPending {
        pending: Publication,
        this_msg: lb_zone_sdk::node_types::MsgId,
        checkpoint: Box<SequencerCheckpoint>,
    },
}

/// State owned exclusively by the participant's background task.
struct Runtime {
    db: Databases,
    channel_id: ChannelId,
    read_only: bool,
    max_batch_transactions: NonZeroU16,
    command_rx: mpsc::Receiver<Command>,
    sequencer_ready: bool,
    ready_checkpoint_pending: bool,
    ready_tx: Option<oneshot::Sender<()>>,
    event_pending_retry: Option<PendingEvent>,
    publish_state: PublishState,
    next_publish_at: Instant,
}

/// A channel event retained with the error from its latest application attempt.
struct PendingEvent {
    event: Event,
    error: Error,
}

impl Runtime {
    async fn run(
        mut self,
        mut sequencer: ZoneSequencer<NodeHttpClient>,
        restored_checkpoint: Option<SequencerCheckpoint>,
    ) -> Result<(), Error> {
        self.recover_published_write(restored_checkpoint.as_ref())?;

        let mut retry = interval(PUBLISH_RETRY_INTERVAL);
        retry.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            {
                // Keep the same SDK event future across commands. Restarting it
                // for each write can starve channel processing under sustained load.
                // Leave this scope before publishing to release the sequencer borrow.
                let event = sequencer.next_event();
                tokio::pin!(event);

                loop {
                    let retry_event = self.event_pending_retry.is_some();
                    let save_publication =
                        matches!(self.publish_state, PublishState::CheckpointPending { .. });

                    // Finish the previous event before taking another. Also save
                    // the publication checkpoint before polling the SDK, since
                    // polling sends queued publications to the node.
                    let ready_for_events = !retry_event && !save_publication;

                    tokio::select! {
                        event = &mut event, if ready_for_events => {
                            self.handle_event(event);
                            break;
                        }
                        command = self.command_rx.recv() => {
                            let Some(command) = command else {
                                return self.shutdown_result();
                            };

                            if !self.handle_command(command) {
                                return self.shutdown_result();
                            }

                            if !ready_for_events {
                                break;
                            }
                        }
                        _ = retry.tick(), if retry_event => {
                            self.retry_pending_event()?;
                            break;
                        }
                        () = sleep_until(self.next_publish_at),
                            if save_publication && !retry_event && self.can_publish() => break,
                    }
                }
            }

            // Publish against fully applied channel history, after the batch
            // collection delay or failure backoff has elapsed.
            let event_applied = self.event_pending_retry.is_none();
            let publish_delay_elapsed = Instant::now() >= self.next_publish_at;

            if self.can_publish() && event_applied && publish_delay_elapsed {
                let save_publication =
                    matches!(self.publish_state, PublishState::CheckpointPending { .. });

                // An accepted publication must be saved even if the queue is empty.
                if save_publication || self.db.has_pending_writes()? {
                    self.publish_queued_writes(&mut sequencer).await;
                }
            }
        }
    }

    fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::Execute {
                tx_id,
                transaction,
                response_tx,
            } => {
                let result = self.execute(tx_id, &transaction);
                drop(response_tx.send(result));

                true
            }
            Command::RetryDisplacement {
                displacement,
                response_tx,
            } => {
                let result = self.retry_displacement(&displacement);
                drop(response_tx.send(result));

                true
            }
            Command::WriteStatus { tx_id, response_tx } => {
                let result = if self.event_pending_retry.is_some() {
                    Err(Error::RuntimeHalted)
                } else {
                    self.db.write_status(tx_id)
                };
                drop(response_tx.send(result));

                true
            }
            Command::HandleDisplacement {
                displacement,
                response_tx,
            } => {
                let result = if self.event_pending_retry.is_some() {
                    Err(Error::RuntimeHalted)
                } else {
                    self.db.mark_displacement_handled(&displacement)
                };
                drop(response_tx.send(result));

                true
            }
            Command::UnhandledDisplacements { response_tx } => {
                let result = if self.event_pending_retry.is_some() {
                    Err(Error::RuntimeHalted)
                } else {
                    self.db.unhandled_displacements()
                };

                drop(response_tx.send(result));

                true
            }
            Command::Shutdown => false,
        }
    }

    fn execute(&mut self, tx_id: TxId, transaction: &Transaction) -> Result<TxId, Error> {
        self.ensure_ready_to_write()?;

        // No await between checking and committing: a channel event cannot
        // displace local state between these two operations.
        if self.db.has_unhandled_displacements()? {
            return Err(Error::UnhandledDisplacements);
        }

        let queue_was_empty = !self.db.has_pending_writes()?;
        self.db.commit_local_write(tx_id, transaction)?;

        if queue_was_empty {
            self.next_publish_at = Instant::now() + BATCH_DELAY;
        }

        tracing::trace!(target: TARGET, ?tx_id, "local write queued for publication");

        Ok(tx_id)
    }

    /// An explicit retry may write while other displacements await handling.
    fn retry_displacement(&mut self, displacement: &Displacement) -> Result<TxId, Error> {
        self.ensure_ready_to_write()?;

        // Check the exact occurrence before committing, without yielding to
        // channel events that could restore or displace the write again.
        if !self.db.is_unhandled_displacement(displacement)? {
            return Err(Error::StaleDisplacement);
        }

        let tx_id = TxId::generate();
        let queue_was_empty = !self.db.has_pending_writes()?;
        self.db
            .commit_local_write(tx_id, &displacement.transaction)?;

        if queue_was_empty {
            self.next_publish_at = Instant::now() + BATCH_DELAY;
        }

        // This control.db update is separate from the LIVE.db commit.
        // A failure here does not roll back the retry's SQL changes.
        self.db.mark_displacement_handled(displacement)?;

        Ok(tx_id)
    }

    const fn ensure_ready_to_write(&self) -> Result<(), Error> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }

        if self.event_pending_retry.is_some() {
            return Err(Error::RuntimeHalted);
        }

        if matches!(self.publish_state, PublishState::CheckpointPending { .. }) {
            return Err(Error::PublishPending);
        }

        if !self.sequencer_ready || self.ready_checkpoint_pending {
            return Err(Error::SequencerNotReady);
        }

        Ok(())
    }

    /// Publication failures leave the committed write pending for retry.
    async fn publish_queued_writes(&mut self, sequencer: &mut ZoneSequencer<NodeHttpClient>) {
        let result = self
            .advance_publish(sequencer)
            .await
            .and_then(|()| self.db.pending_write_count());

        let delay = match result {
            Ok(queued) if queued >= usize::from(self.max_batch_transactions.get()) => {
                Duration::ZERO
            }
            Ok(_) => BATCH_DELAY,
            Err(error) => {
                tracing::warn!(
                    target: TARGET,
                    %error,
                    "committed writes remain queued for publication"
                );

                PUBLISH_RETRY_INTERVAL
            }
        };

        // A full batch needs no collection delay. Smaller batches still get
        // time to fill; the run loop drives the SDK before either is published.
        self.next_publish_at = Instant::now() + delay;
    }

    fn handle_event(&mut self, event: Event) {
        let result = applier::on_event(&mut self.db, &event, self.channel_id);

        match result {
            Ok(()) => {
                self.record_applied_event(&event);
            }
            Err(error) => {
                tracing::error!(target: TARGET, %error, "applier halted");
                self.event_pending_retry = Some(PendingEvent { event, error });
            }
        }
    }

    fn retry_pending_event(&mut self) -> Result<(), Error> {
        if let Some(pending) = self.event_pending_retry.take() {
            return self.retry_event(pending);
        }

        Ok(())
    }

    fn retry_event(&mut self, pending: PendingEvent) -> Result<(), Error> {
        let event = pending.event;

        match applier::on_event(&mut self.db, &event, self.channel_id) {
            Ok(()) => {}
            Err(error) => {
                if !is_retryable_apply_error(&error) {
                    return Err(error);
                }

                tracing::debug!(target: TARGET, %error, "applier retry failed");
                self.event_pending_retry = Some(PendingEvent { event, error });
                return Ok(());
            }
        }
        self.record_applied_event(&event);

        Ok(())
    }

    fn record_applied_event(&mut self, event: &Event) {
        match event {
            Event::Ready => {
                // ZoneSDK queues the checkpoint for the block that made it
                // ready behind this event. Publishing before consuming that
                // checkpoint would let the older buffered value overwrite the
                // checkpoint returned by the publish.
                self.ready_checkpoint_pending = true;
            }
            Event::BlocksProcessed { .. } if self.ready_checkpoint_pending => {
                self.ready_checkpoint_pending = false;

                if !self.sequencer_ready {
                    self.sequencer_ready = true;

                    if let Some(ready_tx) = self.ready_tx.take() {
                        let _ = ready_tx.send(());
                    }
                }
            }
            Event::BlocksProcessed { .. }
            | Event::MempoolPending(_)
            | Event::TurnNotification { .. } => {}
        }
    }

    const fn can_publish(&self) -> bool {
        !self.read_only && self.sequencer_ready && !self.ready_checkpoint_pending
    }

    async fn advance_publish(
        &mut self,
        sequencer: &mut ZoneSequencer<NodeHttpClient>,
    ) -> Result<(), Error> {
        self.persist_publish_checkpoint()?;

        let Some(pending) =
            Publication::prepare(self.db.pending_writes()?, self.max_batch_transactions)?
        else {
            return Ok(());
        };

        let inscription: Inscription = pending
            .payload
            .clone()
            .try_into()
            .map_err(|_| Error::InscriptionTooLarge)?;

        let (published, checkpoint) = sequencer.handle().publish(inscription).await?;
        let this_msg = published.tx.inscription().this_msg;

        tracing::trace!(
            target: TARGET,
            writes = pending.writes.len(),
            "batch accepted by ZoneSDK"
        );

        self.publish_state = PublishState::CheckpointPending {
            pending,
            this_msg,
            checkpoint: Box::new(checkpoint),
        };

        self.persist_publish_checkpoint()
    }

    fn persist_publish_checkpoint(&mut self) -> Result<(), Error> {
        let PublishState::CheckpointPending {
            pending,
            this_msg,
            checkpoint,
        } = &self.publish_state
        else {
            return Ok(());
        };

        self.db
            .complete_batch(checkpoint, *this_msg, &pending.writes)?;

        tracing::trace!(
            target: TARGET,
            writes = pending.writes.len(),
            "batch publication recorded"
        );

        self.publish_state = PublishState::Idle;

        Ok(())
    }

    fn recover_published_write(
        &mut self,
        checkpoint: Option<&SequencerCheckpoint>,
    ) -> Result<(), Error> {
        let pending = self.db.pending_writes()?;

        let Some(checkpoint) = checkpoint else {
            return Ok(());
        };

        for inscription in checkpoint
            .pending_txs
            .iter()
            .flat_map(|(_, transaction)| channel_inscriptions(transaction, self.channel_id))
        {
            let Some(count) = Publication::matches_pending(inscription.payload.as_ref(), &pending)?
            else {
                continue;
            };
            self.db
                .complete_batch(checkpoint, inscription.this_msg, &pending[..count])?;

            tracing::debug!(
                target: TARGET,
                writes = count,
                "restored ZoneSDK checkpoint matched queued batch"
            );
            break;
        }

        Ok(())
    }

    fn shutdown_result(&mut self) -> Result<(), Error> {
        match self.event_pending_retry.take() {
            Some(pending) => Err(pending.error),
            None => Ok(()),
        }
    }
}

const fn is_retryable_apply_error(error: &Error) -> bool {
    !matches!(error, Error::InvalidLocalState(_))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lb_key_management_system_service::keys::Ed25519Key;
    use lb_zone_sdk::{
        CommonHttpClient,
        adapter::NodeHttpClient,
        node_types::{ChannelId, HeaderId, MsgId, Slot, TxHash},
        sequencer::{
            ChannelUpdate, ChannelUpdateTx, Event, FundingConfig, InscriptionInfo,
            SequencerCheckpoint, ZoneSequencer,
        },
    };
    use rusqlite::Connection;
    use tempfile::TempDir;
    use tokio::{
        io::AsyncReadExt as _,
        net::TcpListener,
        sync::{mpsc, oneshot},
        time::timeout,
    };

    use super::{COMMAND_CHANNEL_CAPACITY, Command, PendingEvent, PublishState, Runtime};
    use crate::{
        PublicationConfig,
        db::{Databases, tests::open_databases},
        error::Error,
        publication::Publication,
        sql::TransactionBuilder,
        status::WriteStatus,
    };

    #[tokio::test]
    async fn commands_do_not_cancel_an_unfinished_event() {
        let (_dir, mut runtime, _) = runtime();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        runtime.command_rx = command_rx;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sequencer = sequencer_at(&format!("http://{}", listener.local_addr().unwrap()));

        let commands = async {
            // Hold the SDK's first node request open while commands arrive.
            // Restarting next_event would abandon it and issue another request.
            let (mut connection, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();

            while !request.ends_with(b"\r\n\r\n") {
                request.push(connection.read_u8().await.unwrap());
            }

            for _ in 0..4 {
                let (response_tx, response_rx) = oneshot::channel();
                command_tx
                    .send(Command::UnhandledDisplacements { response_tx })
                    .await
                    .unwrap();
                assert!(response_rx.await.unwrap().unwrap().is_empty());
            }

            let restarted = timeout(Duration::from_millis(250), listener.accept())
                .await
                .is_ok();
            command_tx.send(Command::Shutdown).await.unwrap();

            restarted
        };

        let (result, restarted) = timeout(Duration::from_secs(5), async {
            tokio::join!(runtime.run(sequencer, None), commands)
        })
        .await
        .expect("commands and the event should both complete");

        result.unwrap();
        assert!(
            !restarted,
            "commands must not restart the SDK's node request"
        );
    }

    #[tokio::test]
    async fn shutdown_does_not_wait_for_an_event() {
        let (_dir, mut runtime, _) = runtime();
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        runtime.command_rx = command_rx;
        command_tx.send(Command::Shutdown).await.unwrap();

        let result = timeout(Duration::from_secs(5), runtime.run(sequencer(), None))
            .await
            .expect("shutdown should interrupt the wait");

        result.unwrap();
    }

    #[tokio::test]
    async fn writes_queue_locally_and_survive_a_publication_failure() {
        let (dir, mut runtime, _) = runtime();
        runtime.sequencer_ready = true;
        let mut ids = Vec::new();

        for sql in [
            "CREATE TABLE items(value INTEGER)",
            "INSERT INTO items VALUES (1)",
            "INSERT INTO items VALUES (2)",
        ] {
            let (tx_id, transaction) = TransactionBuilder::new(sql).finish().unwrap();
            runtime.execute(tx_id, &transaction).unwrap();
            ids.push(tx_id);
        }

        for tx_id in &ids {
            assert_eq!(
                runtime.db.write_status(*tx_id).unwrap(),
                Some(WriteStatus::Live)
            );
        }

        // This test sequencer has no node connection and cannot accept a publish.
        runtime.publish_queued_writes(&mut sequencer()).await;
        assert_eq!(runtime.db.pending_writes().unwrap().len(), 3);
        drop(runtime);

        let db = open_databases(dir.path()).unwrap();
        let publication = Publication::prepare(
            db.pending_writes().unwrap(),
            PublicationConfig::default().max_transactions,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            publication
                .writes
                .iter()
                .map(|write| write.tx_id)
                .collect::<Vec<_>>(),
            ids
        );
        let connection = Databases::open_reader(db.live_path()).unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn adopting_our_batch_keeps_later_queued_writes() {
        let (_dir, mut runtime, _) = runtime();
        runtime.sequencer_ready = true;

        for sql in [
            "CREATE TABLE items(value INTEGER)",
            "INSERT INTO items VALUES (1)",
        ] {
            let (tx_id, transaction) = TransactionBuilder::new(sql).finish().unwrap();
            runtime.execute(tx_id, &transaction).unwrap();
        }

        let publication = Publication::prepare(
            runtime.db.pending_writes().unwrap(),
            PublicationConfig::default().max_transactions,
        )
        .unwrap()
        .unwrap();
        let mut event = blocks_processed();
        let Event::BlocksProcessed {
            checkpoint,
            channel_update,
            ..
        } = &mut event
        else {
            unreachable!()
        };
        let this_msg = MsgId::from([4; 32]);
        runtime
            .db
            .complete_batch(checkpoint, this_msg, &publication.writes)
            .unwrap();
        channel_update
            .adopted
            .push(ChannelUpdateTx::Inscription(InscriptionInfo {
                tx_hash: TxHash::from([4; 32]),
                parent_msg: MsgId::root(),
                this_msg,
                payload: publication.payload.try_into().unwrap(),
                signer: None,
            }));

        let (tx_id, transaction) = TransactionBuilder::new("INSERT INTO items VALUES (2)")
            .finish()
            .unwrap();
        runtime.execute(tx_id, &transaction).unwrap();
        runtime.handle_event(event);

        assert!(runtime.event_pending_retry.is_none());
        assert!(runtime.db.unhandled_displacements().unwrap().is_empty());
        assert_eq!(runtime.db.pending_writes().unwrap()[0].tx_id, tx_id);
        let connection = Databases::open_reader(runtime.db.live_path()).unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn read_only_accepts_channel_writes_only() {
        let (_writer_dir, mut writer, _) = runtime();
        let inscription = published_local_write(&mut writer, 2);
        let (_reader_dir, mut reader, _) = runtime();
        reader.read_only = true;
        reader.sequencer_ready = true;

        let mut event = blocks_processed();
        let Event::BlocksProcessed { channel_update, .. } = &mut event else {
            unreachable!()
        };

        channel_update
            .adopted
            .push(ChannelUpdateTx::Inscription(inscription.clone()));
        reader.handle_event(event);

        let connection = Databases::open_reader(reader.db.live_path()).unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM local_2", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(reader.event_pending_retry.is_none());
        assert!(!reader.can_publish());

        let (tx_id, transaction) = prepare_transaction("CREATE TABLE forbidden(value INTEGER)")
            .finish()
            .unwrap();

        assert!(matches!(
            reader.execute(tx_id, &transaction),
            Err(Error::ReadOnly)
        ));
        assert!(
            connection
                .query_row("SELECT count(*) FROM forbidden", [], |row| row
                    .get::<_, i64>(0))
                .is_err()
        );

        writer.handle_event(orphan_event(inscription));
        let displacement = writer.db.unhandled_displacements().unwrap().remove(0);

        assert!(matches!(
            reader.retry_displacement(&displacement),
            Err(Error::ReadOnly)
        ));
        assert!(reader.db.pending_publish().unwrap().is_none());
    }

    #[tokio::test]
    async fn retry_handles_only_the_selected_displacement() {
        let (_dir, mut runtime, _) = runtime();
        let first = published_local_write(&mut runtime, 2);
        let second = published_local_write(&mut runtime, 3);
        runtime.handle_event(orphan_event(first));
        runtime.handle_event(orphan_event(second));
        let displacements = runtime.db.unhandled_displacements().unwrap();
        let displacement = &displacements[0];

        let (response_tx, response_rx) = oneshot::channel();
        runtime.handle_command(Command::RetryDisplacement {
            displacement: displacement.clone(),
            response_tx,
        });
        assert!(matches!(
            response_rx.await.unwrap(),
            Err(Error::SequencerNotReady)
        ));
        assert_eq!(runtime.db.unhandled_displacements().unwrap(), displacements);

        runtime.sequencer_ready = true;
        let (response_tx, response_rx) = oneshot::channel();
        runtime.handle_command(Command::RetryDisplacement {
            displacement: displacement.clone(),
            response_tx,
        });
        let retry_id = response_rx.await.unwrap().unwrap();

        assert_ne!(retry_id, displacement.tx_id);
        assert_eq!(
            runtime.db.write_status(retry_id).unwrap(),
            Some(WriteStatus::Live)
        );
        assert_eq!(
            runtime.db.unhandled_displacements().unwrap(),
            vec![displacements[1].clone()]
        );

        let (tx_id, transaction) = prepare_transaction("CREATE TABLE ordinary(value INTEGER)")
            .finish()
            .unwrap();
        assert!(matches!(
            runtime.execute(tx_id, &transaction),
            Err(Error::UnhandledDisplacements)
        ));
    }

    #[tokio::test]
    async fn failed_retry_keeps_the_displacement_available_for_review() {
        let (_dir, mut runtime, _) = runtime();
        let original = published_local_write(&mut runtime, 2);
        runtime.handle_event(orphan_event(original.clone()));
        let displacement = runtime.db.unhandled_displacements().unwrap().remove(0);

        // The table now exists, but the original write is still displaced.
        Connection::open(runtime.db.live_path())
            .unwrap()
            .execute_batch("CREATE TABLE local_2(value INTEGER)")
            .unwrap();
        runtime.sequencer_ready = true;

        let (response_tx, response_rx) = oneshot::channel();
        runtime.handle_command(Command::RetryDisplacement {
            displacement: displacement.clone(),
            response_tx,
        });

        assert!(matches!(
            response_rx.await.unwrap(),
            Err(Error::Database(_))
        ));
        assert_eq!(
            runtime.db.unhandled_displacements().unwrap(),
            vec![displacement]
        );
        assert!(runtime.db.pending_publish().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_saved_displacement_cannot_be_retried_after_it_is_cleared() {
        for restored in [false, true] {
            let (_dir, mut runtime, _) = runtime();
            let original = published_local_write(&mut runtime, 2);
            runtime.handle_event(orphan_event(original.clone()));
            let displacement = runtime.db.unhandled_displacements().unwrap().remove(0);

            if restored {
                let mut event = blocks_processed();
                let Event::BlocksProcessed { channel_update, .. } = &mut event else {
                    unreachable!()
                };
                channel_update
                    .adopted
                    .push(ChannelUpdateTx::Inscription(original));
                runtime.handle_event(event);
            } else {
                runtime.db.mark_displacement_handled(&displacement).unwrap();
            }

            runtime.sequencer_ready = true;

            assert!(matches!(
                runtime.retry_displacement(&displacement),
                Err(Error::StaleDisplacement)
            ));
            assert!(runtime.db.pending_publish().unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn an_old_displacement_cannot_retry_a_new_occurrence() {
        let (_dir, mut runtime, _) = runtime();
        let original = published_local_write(&mut runtime, 2);
        runtime.handle_event(orphan_event(original.clone()));
        let old = runtime.db.unhandled_displacements().unwrap().remove(0);

        let mut event = blocks_processed();
        let Event::BlocksProcessed { channel_update, .. } = &mut event else {
            unreachable!()
        };
        channel_update
            .adopted
            .push(ChannelUpdateTx::Inscription(original.clone()));
        runtime.handle_event(event);
        runtime.handle_event(orphan_event(original));
        let current = runtime.db.unhandled_displacements().unwrap();
        runtime.sequencer_ready = true;

        assert!(matches!(
            runtime.retry_displacement(&old),
            Err(Error::StaleDisplacement)
        ));
        assert_eq!(runtime.db.unhandled_displacements().unwrap(), current);
        assert!(runtime.db.pending_publish().unwrap().is_none());
    }

    #[tokio::test]
    async fn writes_resume_only_after_all_displacements_are_handled() {
        let (_dir, mut runtime, _) = runtime();
        runtime.sequencer_ready = true;
        let first = published_local_write(&mut runtime, 2);
        let second = published_local_write(&mut runtime, 3);
        runtime.handle_event(orphan_event(first));
        let first = runtime.db.unhandled_displacements().unwrap()[0].clone();

        let (tx_id, transaction) = TransactionBuilder::new("CREATE TABLE resumed(value INTEGER)")
            .finish()
            .unwrap();
        assert!(matches!(
            runtime.execute(tx_id, &transaction),
            Err(Error::UnhandledDisplacements)
        ));
        assert!(runtime.db.pending_publish().unwrap().is_none());

        runtime.handle_event(orphan_event(second));
        runtime.db.mark_displacement_handled(&first).unwrap();
        let (tx_id, transaction) = TransactionBuilder::new("CREATE TABLE resumed(value INTEGER)")
            .finish()
            .unwrap();
        assert!(matches!(
            runtime.execute(tx_id, &transaction),
            Err(Error::UnhandledDisplacements)
        ));

        let remaining = runtime.db.unhandled_displacements().unwrap();
        assert_eq!(remaining.len(), 1);
        let (response_tx, response_rx) = oneshot::channel();
        runtime.handle_command(Command::HandleDisplacement {
            displacement: remaining[0].clone(),
            response_tx,
        });
        response_rx.await.unwrap().unwrap();

        let (tx_id, transaction) = TransactionBuilder::new("CREATE TABLE resumed(value INTEGER)")
            .finish()
            .unwrap();
        assert_eq!(runtime.execute(tx_id, &transaction).unwrap(), tx_id);
        assert_eq!(
            runtime.db.write_status(first.tx_id).unwrap(),
            Some(WriteStatus::Displaced)
        );
    }

    fn prepare_transaction(sql: impl Into<String>) -> TransactionBuilder {
        TransactionBuilder::new(sql)
    }

    fn published_local_write(runtime: &mut Runtime, position: u8) -> InscriptionInfo {
        let (tx_id, transaction) =
            prepare_transaction(format!("CREATE TABLE local_{position}(value INTEGER)"))
                .finish()
                .unwrap();

        runtime.db.commit_local_write(tx_id, &transaction).unwrap();
        let pending = runtime.db.pending_publish().unwrap().unwrap();

        let Event::BlocksProcessed { checkpoint, .. } = blocks_processed() else {
            unreachable!()
        };
        let this_msg = MsgId::from([position; 32]);

        runtime
            .db
            .complete_publish(&checkpoint, this_msg, &pending)
            .unwrap();

        InscriptionInfo {
            tx_hash: TxHash::from([position; 32]),
            parent_msg: MsgId::root(),
            this_msg,
            payload: pending.payload.try_into().unwrap(),
            signer: None,
        }
    }

    fn orphan_event(inscription: InscriptionInfo) -> Event {
        let mut event = blocks_processed();
        let Event::BlocksProcessed { channel_update, .. } = &mut event else {
            unreachable!()
        };

        channel_update
            .orphaned
            .push(ChannelUpdateTx::Inscription(inscription));

        event
    }

    #[tokio::test]
    async fn displacement_remains_unhandled_after_checkpoint_recovery() {
        let (dir, mut runtime, _ready_rx) = runtime();
        let (tx_id, transaction) = prepare_transaction("CREATE TABLE local_write(value INTEGER)")
            .finish()
            .expect("transaction should be valid");
        runtime
            .db
            .commit_local_write(tx_id, &transaction)
            .expect("write should commit");
        let pending = runtime
            .db
            .pending_publish()
            .expect("pending write should load")
            .unwrap();
        let inscription = InscriptionInfo {
            tx_hash: TxHash::from([2; 32]),
            parent_msg: MsgId::root(),
            this_msg: MsgId::from([2; 32]),
            payload: pending
                .payload
                .clone()
                .try_into()
                .expect("payload should fit"),
            signer: None,
        };
        let mut event = blocks_processed();
        let Event::BlocksProcessed {
            checkpoint,
            channel_update,
            ..
        } = &mut event
        else {
            unreachable!()
        };
        runtime
            .db
            .complete_publish(checkpoint, inscription.this_msg, &pending)
            .expect("publication should be recorded");
        channel_update
            .orphaned
            .push(ChannelUpdateTx::Inscription(inscription));

        let control =
            Connection::open(dir.path().join("control.db")).expect("control database should open");
        control
            .execute_batch(
                "CREATE TRIGGER fail_checkpoint BEFORE UPDATE OF checkpoint ON __logos_sql_state
             BEGIN SELECT RAISE(FAIL, 'test checkpoint failure'); END;",
            )
            .expect("failure should be installed");

        runtime.handle_event(event);

        assert!(runtime.event_pending_retry.is_some());
        assert_eq!(
            runtime.db.write_status(tx_id).unwrap(),
            Some(WriteStatus::Displaced)
        );

        control
            .execute_batch("DROP TRIGGER fail_checkpoint")
            .expect("failure should be removed");
        runtime.retry_pending_event().expect("event should recover");

        assert!(runtime.event_pending_retry.is_none());
        assert!(runtime.db.has_unhandled_displacements().unwrap());
    }

    #[tokio::test]
    async fn writes_resume_after_the_publication_checkpoint_is_saved() {
        let (dir, mut runtime, _) = runtime();
        runtime.sequencer_ready = true;
        let (tx_id, transaction) = TransactionBuilder::new("CREATE TABLE items(value INTEGER)")
            .finish()
            .unwrap();
        runtime.execute(tx_id, &transaction).unwrap();

        let pending = Publication::prepare(
            runtime.db.pending_writes().unwrap(),
            PublicationConfig::default().max_transactions,
        )
        .unwrap()
        .unwrap();
        let Event::BlocksProcessed { checkpoint, .. } = blocks_processed() else {
            unreachable!()
        };
        runtime.publish_state = PublishState::CheckpointPending {
            pending,
            this_msg: MsgId::from([2; 32]),
            checkpoint: Box::new(checkpoint),
        };

        let control = Connection::open(dir.path().join("control.db")).unwrap();
        control
            .execute_batch(
                "CREATE TRIGGER fail_checkpoint BEFORE UPDATE OF checkpoint ON __logos_sql_state
                 BEGIN SELECT RAISE(FAIL, 'test checkpoint failure'); END;",
            )
            .unwrap();

        assert!(runtime.persist_publish_checkpoint().is_err());
        let (next_id, next_write) = TransactionBuilder::new("INSERT INTO items VALUES (1)")
            .finish()
            .unwrap();
        assert!(matches!(
            runtime.execute(next_id, &next_write),
            Err(Error::PublishPending)
        ));

        control
            .execute_batch("DROP TRIGGER fail_checkpoint")
            .unwrap();
        runtime.persist_publish_checkpoint().unwrap();
        runtime.execute(next_id, &next_write).unwrap();

        assert!(matches!(runtime.publish_state, PublishState::Idle));
        assert_eq!(runtime.db.pending_writes().unwrap()[0].tx_id, next_id);
    }

    #[tokio::test]
    async fn ready_waits_for_its_block_checkpoint_before_enabling_writes() {
        let (_dir, mut runtime, ready_rx) = runtime();

        runtime.record_applied_event(&Event::Ready);

        assert!(!runtime.can_publish());

        runtime.record_applied_event(&blocks_processed());

        assert!(runtime.can_publish());
        ready_rx.await.expect("runtime should announce readiness");

        runtime.record_applied_event(&Event::Ready);

        assert!(!runtime.can_publish());

        runtime.record_applied_event(&blocks_processed());

        assert!(runtime.can_publish());
    }

    #[tokio::test]
    async fn shutdown_returns_the_pending_applier_error() {
        let (_dir, mut runtime, _ready_rx) = runtime();
        runtime.event_pending_retry = Some(PendingEvent {
            event: blocks_processed(),
            error: Error::InvalidLocalState("test applier failure"),
        });

        let error = runtime
            .shutdown_result()
            .expect_err("shutdown should expose the applier failure");

        assert!(matches!(
            error,
            Error::InvalidLocalState("test applier failure")
        ));
    }

    fn runtime() -> (TempDir, Runtime, oneshot::Receiver<()>) {
        let dir = TempDir::new().expect("temporary directory should be created");
        let db = open_databases(dir.path()).expect("databases should open");
        let channel_id = ChannelId::from([9; 32]);
        let (_command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let runtime = Runtime {
            db,
            channel_id,
            read_only: false,
            max_batch_transactions: PublicationConfig::default().max_transactions,
            command_rx,
            sequencer_ready: false,
            ready_checkpoint_pending: false,
            ready_tx: Some(ready_tx),
            event_pending_retry: None,
            publish_state: PublishState::Idle,
            next_publish_at: tokio::time::Instant::now() + super::BATCH_DELAY,
        };

        (dir, runtime, ready_rx)
    }

    fn sequencer() -> ZoneSequencer<NodeHttpClient> {
        sequencer_at("http://127.0.0.1:1")
    }

    fn sequencer_at(url: &str) -> ZoneSequencer<NodeHttpClient> {
        let node = NodeHttpClient::new(
            CommonHttpClient::new(None),
            url.parse().expect("test node URL should parse"),
        );
        ZoneSequencer::init(
            ChannelId::from([9; 32]),
            Ed25519Key::from_bytes(&[7; 32]),
            node,
            FundingConfig {
                funding_pk: lb_groth16::Fr::from(1u64).into(),
                change_pk: None,
                max_tx_fee: u64::MAX.into(),
                priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
            },
            None,
        )
    }

    fn blocks_processed() -> Event {
        Event::BlocksProcessed {
            checkpoint: SequencerCheckpoint {
                last_msg_id: MsgId::root(),
                pending_txs: Vec::new(),
                lib: HeaderId::from([1; 32]),
                lib_slot: Slot::from(1),
                channel_notes: Vec::new(),
                finalized_config: MsgId::root(),
            },
            channel_update: ChannelUpdate {
                common_prefix: Vec::new(),
                adopted: Vec::new(),
                orphaned: Vec::new(),
                adopted_deposits: Vec::new(),
            },
            finalized: Vec::new(),
        }
    }
}
