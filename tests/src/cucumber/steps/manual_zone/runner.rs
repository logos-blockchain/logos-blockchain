//! Single per-scenario drive loop for a
//! [`lb_zone_sdk::sequencer::ZoneSequencer`] plus a cloneable cross-task
//! command surface for cucumber step functions.
//!
//! The drive task owns the sequencer and `tokio::select!`s between:
//!
//! - block stream events (passed to a [`Policy`] inline, then forwarded to a
//!   fan-out mpsc that tests observe)
//! - step-issued commands from [`SequencerClient`] (processed against the same
//!   `&mut sequencer`)
//!
//! Because the policy runs on the same task as event handling, reactions
//! (e.g. orphan republish) happen before the event is forwarded to tests —
//! there is no window where an event was observed but the policy's reaction
//! hasn't been applied to the SDK's state.

use std::marker::PhantomData;

use lb_core::mantle::{
    MantleTx, SignedMantleTx,
    channel::{SlotTimeframe, SlotTimeout},
    encoding::Ops,
    ops::channel::{MsgId, config::Keys, inscribe::Inscription},
};
use lb_key_management_system_service::keys::Ed25519Signature;
pub use lb_zone_sdk::sequencer::{
    AtomicWithdrawInfo, ChannelUpdate, DepositInfo, Error, Event, FinalizedOp, FinalizedTx,
    InscriptionId, InscriptionInfo, OrphanedTx, PendingTx, PublishResult, SequencerChannelView,
    SequencerCheckpoint, SequencerConfig, TurnNotification, TxSource, TxStatus, TxStatusUpdate,
    WithdrawArg, WithdrawInfo,
};
use lb_zone_sdk::{adapter, sequencer::ZoneSequencer};
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    task::JoinHandle,
};

/// Inline policy executed on the drive task for each SDK event before the
/// event is forwarded to test observers.
///
/// Implementations may call `sequencer.handle().publish(...)` to issue
/// state changes; those land on the same task that produced the event,
/// preserving observe-then-act ordering.
pub trait Policy<Node>: Send + 'static
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    fn on_event(
        &mut self,
        sequencer: &mut ZoneSequencer<Node>,
        event: &Event,
    ) -> impl Future<Output = ()> + Send;
}

/// No-op policy — used when the test wants to drive the sequencer entirely
/// from step bodies via [`SequencerClient`].
pub struct PassivePolicy;

impl<Node> Policy<Node> for PassivePolicy
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, _sequencer: &mut ZoneSequencer<Node>, _event: &Event) {}
}

/// Step-issued command dispatched against the sequencer by the drive task.
pub(super) enum Cmd {
    Publish {
        data: Inscription,
        reply: oneshot::Sender<Result<(PublishResult, SequencerCheckpoint), Error>>,
    },
    PublishAtomicWithdraw {
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
        reply: oneshot::Sender<Result<(PublishResult, SequencerCheckpoint), Error>>,
    },
    ChannelConfig {
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
        reply: oneshot::Sender<Result<(PublishResult, SequencerCheckpoint, SignedMantleTx), Error>>,
    },
    PrepareTx {
        ops: Ops,
        msg: Inscription,
        reply: oneshot::Sender<Result<(MantleTx, MsgId, Ed25519Signature), Error>>,
    },
    SignTx {
        tx: MantleTx,
        reply: oneshot::Sender<Result<Ed25519Signature, Error>>,
    },
    SubmitSignedTx {
        tx: SignedMantleTx,
        msg_id: MsgId,
        reply: oneshot::Sender<Result<(PublishResult, SequencerCheckpoint), Error>>,
    },
}

/// Cloneable cross-task client for issuing commands to the drive task.
///
/// Mirrors the SDK's drive-loop [`lb_zone_sdk::sequencer::SequencerHandle`]
/// at the type level; each method dispatches a [`Cmd`] over an mpsc and
/// awaits the reply. State-mutating methods return the SDK's
/// [`PublishResult`] paired with the resulting [`SequencerCheckpoint`] so
/// step bodies can persist outbox + checkpoint atomically just like a
/// real SDK consumer would.
pub struct SequencerClient<Node> {
    cmd_tx: mpsc::Sender<Cmd>,
    _marker: PhantomData<fn() -> Node>,
}

impl<Node> Clone for SequencerClient<Node> {
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            _marker: PhantomData,
        }
    }
}

const CLOSED: Error = Error::Unavailable {
    reason: "runner: drive task closed",
};
const DROPPED: Error = Error::Unavailable {
    reason: "runner: drive task dropped reply",
};

impl<Node> SequencerClient<Node> {
    pub async fn publish(
        &self,
        data: Inscription,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Publish { data, reply })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }

    pub async fn publish_atomic_withdraw(
        &self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::PublishAtomicWithdraw {
                inscribe,
                withdraws,
                reply,
            })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }

    pub async fn prepare_tx(
        &self,
        ops: Ops,
        msg: Inscription,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::PrepareTx { ops, msg, reply })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }

    pub async fn sign_tx(&self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::SignTx {
                tx: tx.clone(),
                reply,
            })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }

    pub async fn submit_signed_tx(
        &self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<(PublishResult, SequencerCheckpoint), Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::SubmitSignedTx { tx, msg_id, reply })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }

    pub async fn channel_config(
        &self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
    ) -> Result<(PublishResult, SequencerCheckpoint, SignedMantleTx), Error> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::ChannelConfig {
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                withdraw_threshold,
                reply,
            })
            .await
            .map_err(|_| CLOSED)?;
        rx.await.map_err(|_| DROPPED)?
    }
}

/// Bundle returned from [`spawn`].
///
/// Drive task handle + cloneable client + receivers for events and the
/// SDK's watch channels. `event_rx` is the SDK's broadcast subscriber —
/// fire-and-forget; consumers who fall behind get a `RecvError::Lagged`
/// they can recover from.
pub struct Runtime<Node> {
    pub task: JoinHandle<()>,
    pub client: SequencerClient<Node>,
    pub event_rx: broadcast::Receiver<Event>,
    pub checkpoint_rx: watch::Receiver<Option<SequencerCheckpoint>>,
    pub ready_rx: watch::Receiver<bool>,
    pub channel_view_rx: watch::Receiver<SequencerChannelView>,
    pub turn_to_write_rx: watch::Receiver<TurnNotification>,
    pub tx_status_rx: broadcast::Receiver<TxStatusUpdate>,
}

/// Drive loop body. Owns the sequencer until both the block stream
/// disconnects and the command channel closes; for each event invokes
/// the policy inline. Events reach observers via the SDK's own
/// broadcast channel (subscribed in [`spawn`]) — the drive loop does
/// not forward them, so there's no backpressure on the runner from
/// idle subscribers.
///
/// Pulled out as a free async function so callers can spawn it
/// themselves (e.g. onto a custom runtime, a `LocalSet`, or interleaved
/// with other state in the same task). [`spawn`] is the convenience
/// wrapper that creates channels + a tokio task.
pub(super) async fn run<Node, P>(
    mut sequencer: ZoneSequencer<Node>,
    mut policy: P,
    mut cmd_rx: mpsc::Receiver<Cmd>,
) where
    Node: adapter::Node + Clone + Send + Sync + 'static,
    P: Policy<Node>,
{
    loop {
        tokio::select! {
            ev = sequencer.next_event() => {
                let Some(ev) = ev else { continue; };
                policy.on_event(&mut sequencer, &ev).await;
                // Event already broadcast via the SDK's `emit_now`; nothing
                // for the runner to forward.
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break; };
                process_cmd(cmd, &mut sequencer);
            }
        }
    }
}

/// Spawn the drive loop on the current tokio runtime.
///
/// Wires the cmd channel and grabs SDK watch subscriptions before moving
/// the sequencer into the task. Events flow via the SDK's broadcast
/// channel — `event_rx` is a fresh subscriber.
pub fn spawn<Node, P>(sequencer: ZoneSequencer<Node>, policy: P) -> Runtime<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
    P: Policy<Node>,
{
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let checkpoint_rx = sequencer.subscribe_checkpoint();
    let ready_rx = sequencer.subscribe_ready();
    let channel_view_rx = sequencer.subscribe_channel_view();
    let turn_to_write_rx = sequencer.subscribe_turn_to_write();
    let tx_status_rx = sequencer.subscribe_tx_status();
    let event_rx = sequencer.subscribe_events();

    let task = tokio::spawn(run(sequencer, policy, cmd_rx));

    Runtime {
        task,
        client: SequencerClient {
            cmd_tx,
            _marker: PhantomData,
        },
        event_rx,
        checkpoint_rx,
        ready_rx,
        channel_view_rx,
        turn_to_write_rx,
        tx_status_rx,
    }
}

fn process_cmd<Node>(cmd: Cmd, sequencer: &mut ZoneSequencer<Node>)
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    match cmd {
        Cmd::Publish { data, reply } => {
            drop(reply.send(sequencer.handle().publish(data)));
        }
        Cmd::PublishAtomicWithdraw {
            inscribe,
            withdraws,
            reply,
        } => {
            drop(
                reply.send(
                    sequencer
                        .handle()
                        .publish_atomic_withdraw(inscribe, withdraws),
                ),
            );
        }
        Cmd::ChannelConfig {
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            withdraw_threshold,
            reply,
        } => {
            drop(reply.send(sequencer.handle().channel_config(
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                withdraw_threshold,
            )));
        }
        Cmd::PrepareTx { ops, msg, reply } => {
            drop(reply.send(sequencer.handle().prepare_tx(ops, msg)));
        }
        Cmd::SignTx { tx, reply } => {
            drop(reply.send(sequencer.handle().sign_tx(&tx)));
        }
        Cmd::SubmitSignedTx { tx, msg_id, reply } => {
            drop(reply.send(sequencer.handle().submit_signed_tx(tx, msg_id)));
        }
    }
}
