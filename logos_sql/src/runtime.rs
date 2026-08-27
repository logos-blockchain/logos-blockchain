//! Single-owner runtime for SQL writes and channel events.

use std::time::Duration;

use lb_zone_sdk::{
    adapter::NodeHttpClient,
    node_types::{ChannelId, Inscription},
    sequencer::{Event, SequencerCheckpoint, ZoneSequencer, channel_inscriptions},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::{
    applier,
    db::{Databases, PendingPublish},
    error::Error,
    protocol::{Transaction, TxId},
};

const COMMAND_CHANNEL_CAPACITY: usize = 16;
const PUBLISH_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const TARGET: &str = lb_log_targets::logos_sql::RUNTIME;

/// Requests processed by the task that owns the sequencer and database writer.
enum Command {
    Execute {
        tx_id: TxId,
        transaction: Transaction,
        response_tx: oneshot::Sender<Result<TxId, Error>>,
    },
    DisplacedWrites {
        response_tx: oneshot::Sender<Result<Vec<TxId>, Error>>,
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
) -> RuntimeHandle {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (ready_tx, ready_rx) = oneshot::channel();

    let runtime = Runtime {
        sequencer,
        db,
        channel_id,
        command_rx,
        sequencer_ready: false,
        ready_checkpoint_pending: false,
        ready_tx: Some(ready_tx),
        event_pending_retry: None,
        publish_state: PublishState::Idle,
    };
    let task = tokio::spawn(runtime.run(restored_checkpoint));

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

    pub(crate) async fn displaced_writes(&self) -> Result<Vec<TxId>, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::DisplacedWrites { response_tx })
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
        pending: PendingPublish,
        this_msg: lb_zone_sdk::node_types::MsgId,
        checkpoint: Box<SequencerCheckpoint>,
    },
}

/// State owned exclusively by the participant's background task.
struct Runtime {
    sequencer: ZoneSequencer<NodeHttpClient>,
    db: Databases,
    channel_id: ChannelId,
    command_rx: mpsc::Receiver<Command>,
    sequencer_ready: bool,
    ready_checkpoint_pending: bool,
    ready_tx: Option<oneshot::Sender<()>>,
    event_pending_retry: Option<PendingEvent>,
    publish_state: PublishState,
}

/// A channel event retained with the error from its latest application attempt.
struct PendingEvent {
    event: Event,
    error: Error,
}

impl Runtime {
    async fn run(mut self, restored_checkpoint: Option<SequencerCheckpoint>) -> Result<(), Error> {
        self.recover_published_write(restored_checkpoint.as_ref())?;

        let mut retry = tokio::time::interval(PUBLISH_RETRY_INTERVAL);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let retry_needed = self.has_pending_work()?;

            // `ZoneSDK::publish` only queues the node post; `next_event` drives
            // it. Do not poll events until the returned checkpoint commits. A
            // crash while the write remains pending therefore means it never
            // reached the node, which the applier's recovery plan relies on.
            tokio::select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        return Ok(());
                    };

                    if !self.handle_command(command).await {
                        return self.shutdown_result();
                    }
                },
                event = self.sequencer.next_event(), if self.event_pending_retry.is_none() && !matches!(self.publish_state, PublishState::CheckpointPending { .. }) => {
                    self.handle_event(event).await;
                },
                _ = retry.tick(), if retry_needed => {
                    self.retry_pending_work().await?;
                }
            }
        }
    }

    async fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::Execute {
                tx_id,
                transaction,
                response_tx,
            } => {
                let result = self.execute(tx_id, transaction).await;
                drop(response_tx.send(result));

                true
            }
            Command::DisplacedWrites { response_tx } => {
                drop(response_tx.send(self.db.displaced_writes()));

                true
            }
            Command::Shutdown => false,
        }
    }

    async fn execute(&mut self, tx_id: TxId, transaction: Transaction) -> Result<TxId, Error> {
        if self.event_pending_retry.is_some() {
            return Err(Error::RuntimeHalted);
        }

        if !self.sequencer_ready || self.ready_checkpoint_pending {
            return Err(Error::SequencerNotReady);
        }

        self.db.commit_local_write(tx_id, &transaction)?;

        tracing::trace!(
            target: TARGET,
            ?tx_id,
            statements = transaction.statements().len(),
            "local write committed"
        );

        if let Err(error) = self.advance_publish().await {
            tracing::warn!(
                target: TARGET,
                %error,
                ?tx_id,
                "write committed; publication remains pending"
            );
        }

        Ok(tx_id)
    }

    async fn handle_event(&mut self, event: Event) {
        match applier::on_event(&mut self.db, &event, self.channel_id) {
            Ok(()) => {
                self.record_applied_event(&event);

                if self.can_publish()
                    && let Err(error) = self.advance_publish().await
                {
                    tracing::warn!(
                        target: TARGET,
                        %error,
                        "write publication remains pending"
                    );
                }
            }
            Err(error) => {
                tracing::error!(target: TARGET, %error, "applier halted");
                self.event_pending_retry = Some(PendingEvent { event, error });
            }
        }
    }

    async fn retry_pending_work(&mut self) -> Result<(), Error> {
        if let Some(pending) = self.event_pending_retry.take() {
            return self.retry_event(pending).await;
        }

        if self.can_publish()
            && let Err(error) = self.advance_publish().await
        {
            tracing::warn!(target: TARGET, %error, "pending publication retry failed");
        }

        Ok(())
    }

    async fn retry_event(&mut self, pending: PendingEvent) -> Result<(), Error> {
        let event = pending.event;

        if let Err(error) = applier::on_event(&mut self.db, &event, self.channel_id) {
            if !is_retryable_apply_error(&error) {
                return Err(error);
            }

            tracing::debug!(target: TARGET, %error, "applier retry failed");
            self.event_pending_retry = Some(PendingEvent { event, error });
            return Ok(());
        }

        self.record_applied_event(&event);

        if self.can_publish()
            && let Err(error) = self.advance_publish().await
        {
            tracing::warn!(target: TARGET, %error, "pending publication retry failed");
        }

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
        self.sequencer_ready && !self.ready_checkpoint_pending
    }

    async fn advance_publish(&mut self) -> Result<(), Error> {
        self.persist_publish_checkpoint()?;

        let Some(pending) = self.db.pending_publish()? else {
            return Ok(());
        };

        let inscription: Inscription = pending
            .payload
            .clone()
            .try_into()
            .map_err(|_| Error::InscriptionTooLarge)?;

        let (published, checkpoint) = self.sequencer.handle().publish(inscription).await?;
        let this_msg = published.tx.inscription().this_msg;

        tracing::trace!(
            target: TARGET,
            tx_id = ?pending.tx_id,
            "write accepted by ZoneSDK"
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

        self.db.complete_publish(checkpoint, *this_msg, pending)?;

        tracing::trace!(
            target: TARGET,
            tx_id = ?pending.tx_id,
            "write publication recorded"
        );

        self.publish_state = PublishState::Idle;

        Ok(())
    }

    fn recover_published_write(
        &mut self,
        checkpoint: Option<&SequencerCheckpoint>,
    ) -> Result<(), Error> {
        let Some(pending) = self.db.pending_publish()? else {
            return Ok(());
        };

        let Some(checkpoint) = checkpoint else {
            return Ok(());
        };

        let submitted = checkpoint
            .pending_txs
            .iter()
            .flat_map(|(_, transaction)| channel_inscriptions(transaction, self.channel_id))
            .find(|inscription| inscription.payload.as_inner() == &pending.payload);

        if let Some(inscription) = submitted {
            self.db
                .complete_publish(checkpoint, inscription.this_msg, &pending)?;

            tracing::debug!(
                target: TARGET,
                tx_id = ?pending.tx_id,
                "restored ZoneSDK checkpoint matched pending write"
            );
        }

        Ok(())
    }

    fn has_pending_work(&self) -> Result<bool, Error> {
        Ok(self.event_pending_retry.is_some()
            || matches!(self.publish_state, PublishState::CheckpointPending { .. })
            || self.db.pending_publish()?.is_some())
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
    use lb_key_management_system_service::keys::Ed25519Key;
    use lb_zone_sdk::{
        CommonHttpClient,
        adapter::NodeHttpClient,
        node_types::{ChannelId, HeaderId, MsgId, Slot},
        sequencer::{ChannelUpdate, Event, FundingConfig, SequencerCheckpoint, ZoneSequencer},
    };
    use tempfile::TempDir;
    use tokio::sync::{mpsc, oneshot};

    use super::{COMMAND_CHANNEL_CAPACITY, PendingEvent, PublishState, Runtime};
    use crate::{db::Databases, error::Error};

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
        let db = Databases::open(dir.path()).expect("databases should open");
        let channel_id = ChannelId::from([9; 32]);
        let node = NodeHttpClient::new(
            CommonHttpClient::new(None),
            "http://127.0.0.1:1"
                .parse()
                .expect("test node URL should parse"),
        );
        let sequencer = ZoneSequencer::init(
            channel_id,
            Ed25519Key::from_bytes(&[7; 32]),
            node,
            FundingConfig {
                funding_pk: lb_groth16::Fr::from(1u64).into(),
                change_pk: None,
                max_tx_fee: u64::MAX.into(),
                priority_fee_percent: FundingConfig::DEFAULT_PRIORITY_FEE_PERCENT,
            },
            None,
        );
        let (_command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        let runtime = Runtime {
            sequencer,
            db,
            channel_id,
            command_rx,
            sequencer_ready: false,
            ready_checkpoint_pending: false,
            ready_tx: Some(ready_tx),
            event_pending_retry: None,
            publish_state: PublishState::Idle,
        };

        (dir, runtime, ready_rx)
    }

    fn blocks_processed() -> Event {
        Event::BlocksProcessed {
            checkpoint: SequencerCheckpoint {
                last_msg_id: MsgId::root(),
                pending_txs: Vec::new(),
                lib: HeaderId::from([1; 32]),
                lib_slot: Slot::from(1),
                channel_notes: Vec::new(),
            },
            channel_update: ChannelUpdate {
                adopted: Vec::new(),
                orphaned: Vec::new(),
            },
            finalized: Vec::new(),
        }
    }
}
