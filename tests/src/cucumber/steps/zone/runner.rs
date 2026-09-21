//! Single per-scenario drive loop for a
//! [`lb_zone_sdk::sequencer::ZoneSequencer`].
//!
//! The drive task owns the sequencer and polls
//! [`ZoneSequencer::next_event`] in a loop, invoking a [`Policy`] inline for
//! each event. Step bodies issue commands via the SDK's
//! [`SequencerClient`] (handed out from [`Runtime::client`]) — those route
//! through the SDK's internal request channel and get processed by the same
//! drive loop, so reactions land before the next event is observed.
//!
//! Because the policy runs on the same task as event handling, reactions
//! (e.g. orphan republish) happen before the event is forwarded to tests —
//! there is no window where an event was observed but the policy's reaction
//! hasn't been applied to the SDK's state.

use std::collections::HashSet;

use lb_core::mantle::transactions::hash::TxHash;
pub use lb_zone_sdk::sequencer::{
    AtomicWithdrawInfo, ChannelUpdate, ChannelUpdateTx, DepositInfo, Error, Event, FinalizedOp,
    FinalizedTx, FundingConfig, IndexedSignature, InscriptionId, InscriptionInfo, PendingTx,
    PreparedChannelConfig, PublishResult, SequencerChannelView, SequencerCheckpoint,
    SequencerClient, SequencerConfig, TurnNotification, TxSource, TxStatus, TxStatusUpdate,
    WithdrawArg, WithdrawInfo, WithdrawInputs,
};
use lb_zone_sdk::{adapter, sequencer::ZoneSequencer};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tracing::error;

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

/// Bundle returned from [`spawn`].
///
/// Drive task handle + the SDK's cloneable [`SequencerClient`] + receivers
/// for events and the SDK's watch channels. `event_rx` is the SDK's broadcast
/// subscriber — fire-and-forget; consumers who fall behind get a
/// `RecvError::Lagged` they can recover from.
pub struct Runtime {
    pub task: JoinHandle<()>,
    pub client: SequencerClient,
    pub event_rx: broadcast::Receiver<Event>,
    pub checkpoint_rx: watch::Receiver<Option<SequencerCheckpoint>>,
    pub ready_rx: watch::Receiver<bool>,
    pub channel_view_rx: watch::Receiver<SequencerChannelView>,
    pub turn_to_write_rx: watch::Receiver<TurnNotification>,
    pub tx_status_rx: broadcast::Receiver<TxStatusUpdate>,
}

/// Drive loop body. Owns the sequencer and pumps
/// [`ZoneSequencer::next_event`] until aborted. For each event invokes the
/// policy inline. Events reach observers via the SDK's own broadcast channel
/// (subscribed in [`spawn`]) — the drive loop does not forward them.
///
/// Step-issued commands ride through the SDK's internal request channel and
/// are processed inside `next_event`; no separate command path is needed.
///
/// Pulled out as a free async function so callers can spawn it themselves.
pub(super) async fn run<Node, P>(mut sequencer: ZoneSequencer<Node>, mut policy: P)
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
    P: Policy<Node>,
{
    let mut view = ViewChecker::default();
    loop {
        let ev = sequencer.next_event().await;
        view.observe(&ev);
        policy.on_event(&mut sequencer, &ev).await;
        // Event already broadcast via the SDK's `emit_now`; nothing for the
        // runner to forward.
    }
}

/// Asserts the [`ChannelUpdate`] contract on every block event: what a
/// consumer holds from the stream (adopted, not orphaned since, not
/// finalized) must equal `common_prefix ++ adopted`, up to the sequencer's own
/// in-flight publishes, which the prefix carries and the stream never echoes.
#[derive(Default)]
struct ViewChecker {
    held: HashSet<TxHash>,
}

impl ViewChecker {
    fn observe(&mut self, event: &Event) {
        let Event::BlocksProcessed {
            checkpoint,
            channel_update,
            finalized,
        } = event
        else {
            return;
        };
        for tx in &channel_update.orphaned {
            self.held.remove(&tx.tx_hash());
        }
        for tx in &channel_update.adopted {
            self.held.insert(tx.tx_hash());
        }
        for tx in finalized {
            self.held.remove(&tx.tx_hash);
        }

        let view: HashSet<TxHash> = channel_update
            .canonical_chain()
            .map(ChannelUpdateTx::tx_hash)
            .collect();
        let pending: HashSet<TxHash> = checkpoint.pending_txs.iter().map(|(h, _)| *h).collect();
        for tx in &view {
            if !self.held.contains(tx) && !pending.contains(tx) {
                fail(&format!(
                    "common_prefix carries {tx:?}, which was never adopted and is not pending"
                ));
            }
        }
        for tx in &self.held {
            if !view.contains(tx) {
                fail(&format!(
                    "{tx:?} was adopted and never orphaned or finalized, but left common_prefix"
                ));
            }
        }
    }
}

/// Log, then panic: the drive task's panic is swallowed by cucumber's panic
/// hook, so the log line is what the scenario output shows.
fn fail(message: &str) -> ! {
    error!("channel view contract violated: {message}");
    panic!("channel view contract violated: {message}");
}

/// Spawn the drive loop on the current tokio runtime.
///
/// Grabs SDK watch subscriptions and the SDK client before moving the
/// sequencer into the task. Events flow via the SDK's broadcast channel —
/// `event_rx` is a fresh subscriber.
pub fn spawn<Node, P>(sequencer: ZoneSequencer<Node>, policy: P) -> Runtime
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
    P: Policy<Node>,
{
    let checkpoint_rx = sequencer.subscribe_checkpoint();
    let ready_rx = sequencer.subscribe_ready();
    let channel_view_rx = sequencer.subscribe_channel_view();
    let turn_to_write_rx = sequencer.subscribe_turn_to_write();
    let tx_status_rx = sequencer.subscribe_tx_status();
    let event_rx = sequencer.subscribe_events();
    let client = sequencer.client();

    let task = tokio::spawn(run(sequencer, policy));

    Runtime {
        task,
        client,
        event_rx,
        checkpoint_rx,
        ready_rx,
        channel_view_rx,
        turn_to_write_rx,
        tx_status_rx,
    }
}
