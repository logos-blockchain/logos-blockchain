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
    db::Databases,
    error::Error,
    local_write,
    protocol::{IdempotencyKey, Transaction, TxId},
};

const COMMAND_CHANNEL_CAPACITY: usize = 16;
const PUBLISH_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const TARGET: &str = lb_log_targets::lsql::RUNTIME;

/// Requests processed by the task that owns the sequencer and database writer.
enum Command {
    Execute {
        transaction: Transaction,
        idempotency_key: IdempotencyKey,
        response_tx: oneshot::Sender<Result<TxId, Error>>,
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
    writer_id: [u8; 32],
    restored_checkpoint: Option<SequencerCheckpoint>,
) -> RuntimeHandle {
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (ready_tx, ready_rx) = oneshot::channel();

    let runtime = Runtime {
        sequencer,
        db,
        channel_id,
        writer_id,
        command_rx,
        sequencer_ready: false,
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
        transaction: Transaction,
        idempotency_key: IdempotencyKey,
    ) -> Result<TxId, Error> {
        let (response_tx, response_rx) = oneshot::channel();
        self.command_tx
            .send(Command::Execute {
                transaction,
                idempotency_key,
                response_tx,
            })
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
        tx_id: TxId,
        checkpoint: SequencerCheckpoint,
    },
}

/// State owned exclusively by the participant's background task.
struct Runtime {
    sequencer: ZoneSequencer<NodeHttpClient>,
    db: Databases,
    channel_id: ChannelId,
    writer_id: [u8; 32],
    command_rx: mpsc::Receiver<Command>,
    sequencer_ready: bool,
    ready_tx: Option<oneshot::Sender<()>>,
    event_pending_retry: Option<Event>,
    publish_state: PublishState,
}

impl Runtime {
    async fn run(mut self, restored_checkpoint: Option<SequencerCheckpoint>) -> Result<(), Error> {
        self.reconcile_restored_publish(restored_checkpoint.as_ref())?;

        let mut retry = tokio::time::interval(PUBLISH_RETRY_INTERVAL);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let retry_needed = self.has_pending_work()?;

            tokio::select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        return Ok(());
                    };

                    if !self.handle_command(command).await {
                        return Ok(());
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
        let Command::Execute {
            transaction,
            idempotency_key,
            response_tx,
        } = command
        else {
            return false;
        };

        let result = if self.event_pending_retry.is_some() {
            Err(Error::RuntimeHalted)
        } else if !self.sequencer_ready {
            Err(Error::SequencerNotReady)
        } else {
            let committed = local_write::commit(
                &mut self.db,
                &transaction,
                &idempotency_key,
                &self.writer_id,
            );

            if let Ok(tx_id) = committed {
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
                        "write committed; ZoneSDK publish remains pending"
                    );
                }
            }

            committed
        };

        drop(response_tx.send(result));

        true
    }

    async fn handle_event(&mut self, event: Event) {
        match applier::on_event(&mut self.db, &event, self.channel_id) {
            Ok(()) => {
                self.mark_ready(&event);

                if self.sequencer_ready
                    && let Err(error) = self.advance_publish().await
                {
                    tracing::warn!(
                        target: TARGET,
                        %error,
                        "ZoneSDK publish remains pending"
                    );
                }
            }
            Err(error) => {
                tracing::error!(target: TARGET, %error, "applier halted");
                self.event_pending_retry = Some(event);
            }
        }
    }

    async fn retry_pending_work(&mut self) -> Result<(), Error> {
        if let Some(event) = self.event_pending_retry.take() {
            if let Err(error) = applier::on_event(&mut self.db, &event, self.channel_id) {
                if !is_retryable_apply_error(&error) {
                    return Err(error);
                }

                tracing::debug!(target: TARGET, %error, "applier retry failed");
                self.event_pending_retry = Some(event);
            } else {
                self.mark_ready(&event);
            }
        } else if self.sequencer_ready
            && let Err(error) = self.advance_publish().await
        {
            tracing::debug!(target: TARGET, %error, "ZoneSDK publish retry failed");
        }

        Ok(())
    }

    fn mark_ready(&mut self, event: &Event) {
        if self.sequencer_ready || !matches!(event, Event::Ready) {
            return;
        }

        self.sequencer_ready = true;

        if let Some(ready_tx) = self.ready_tx.take() {
            let _ = ready_tx.send(());
        }
    }

    async fn advance_publish(&mut self) -> Result<(), Error> {
        self.persist_publish_checkpoint()?;

        let Some(pending) = self.db.pending_publish()? else {
            return Ok(());
        };

        let inscription: Inscription = pending
            .payload
            .try_into()
            .map_err(|_| Error::InscriptionTooLarge)?;

        let (_, checkpoint) = self.sequencer.handle().publish(inscription).await?;

        tracing::trace!(
            target: TARGET,
            tx_id = ?pending.tx_id,
            "write accepted by ZoneSDK"
        );

        self.publish_state = PublishState::CheckpointPending {
            tx_id: pending.tx_id,
            checkpoint,
        };

        self.persist_publish_checkpoint()
    }

    fn persist_publish_checkpoint(&mut self) -> Result<(), Error> {
        let PublishState::CheckpointPending { tx_id, checkpoint } = &self.publish_state else {
            return Ok(());
        };

        self.db.persist_checkpoint(checkpoint)?;
        self.db.mark_publish_complete(*tx_id)?;

        tracing::trace!(
            target: TARGET,
            ?tx_id,
            "write publication recorded"
        );

        self.publish_state = PublishState::Idle;

        Ok(())
    }

    fn reconcile_restored_publish(
        &self,
        checkpoint: Option<&SequencerCheckpoint>,
    ) -> Result<(), Error> {
        let Some(pending) = self.db.pending_publish()? else {
            return Ok(());
        };

        let Some(checkpoint) = checkpoint else {
            return Ok(());
        };

        let already_submitted = checkpoint.pending_txs.iter().any(|(_, transaction)| {
            channel_inscriptions(transaction, self.channel_id)
                .iter()
                .any(|inscription| inscription.payload.as_ref() == pending.payload)
        });

        if already_submitted {
            self.db.mark_publish_complete(pending.tx_id)?;

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
}

const fn is_retryable_apply_error(error: &Error) -> bool {
    !matches!(
        error,
        Error::InvalidPayload(_) | Error::UnsupportedProtocolVersion(_)
    )
}
