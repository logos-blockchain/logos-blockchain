use std::collections::VecDeque;

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        encoding::Ops,
        ops::{
            Op,
            channel::{ChannelId, MsgId, config::Keys, inscribe::Inscription},
        },
        tx::TxHash,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};
use tokio::sync::{broadcast, mpsc, watch};
use tracing::info;

use super::{
    TARGET,
    handle::SequencerHandle,
    slot_clock::SlotClock,
    state::TxState,
    types::{
        Error, Event, InscriptionId, PublishResult, SequencerChannelView, SequencerCheckpoint,
        SequencerConfig, TurnNotification, WithdrawArg, WithdrawInfo,
    },
};
use crate::{adapter, adapter::BoxStream};

pub(super) enum ActorRequest {
    /// Create/sign/submit a transaction with an inscription
    PublishMessage { data: Inscription },
    /// Build an unsigned tx for the given ops and an inscription
    ///
    /// Calling this multiple times without submitting the prepared txs via
    /// `SubmitSignedTx` can cause parent msg ID conflicts, so ensure
    /// prepared txs are submitted promptly. If additional prepares are
    /// unavoidable, handle potential conflicts carefully.
    PrepareTx {
        ops: Ops,
        msg: Inscription,
        reply: tokio::sync::oneshot::Sender<Result<(MantleTx, MsgId, Ed25519Signature), Error>>,
    },
    /// Sign a tx using the sequencer's key
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw).
    SignTx {
        tx_hash: TxHash,
        reply: tokio::sync::oneshot::Sender<Result<Ed25519Signature, Error>>,
    },
    /// Submit a signed tx associated with a msg ID
    SubmitSignedTx {
        tx: SignedMantleTx,
        msg_id: MsgId,
        reply: tokio::sync::oneshot::Sender<Result<PublishResult, Error>>,
    },
    ChannelConfig {
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        withdraw_threshold: u16,
        reply: tokio::sync::oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
    /// Publish an atomic inscription+withdraw bundle.
    ///
    /// SDK queries channel state to fill `withdraw_nonce`s and locate its own
    /// accredited-key index, builds the `MantleTx`, signs locally, and submits.
    /// Scoped to single-sequencer (centralized) channels — the sequencer's
    /// own signature is the only one used. Fire-and-forget; the result is
    /// delivered via `Event::Published`.
    PublishAtomicWithdraw {
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
    },
}

pub(super) enum InFlight {
    SubmittedBatch {
        results: Vec<(InscriptionId, Result<(), String>)>,
    },
}

/// Zone sequencer.
///
/// The caller drives execution by pumping [`Self::events`] in a loop. Publish
/// and admin operations are submitted via the [`SequencerHandle`] returned
/// from [`Self::init`], which can be cloned and used from any task.
pub struct ZoneSequencer<Node> {
    // Config
    pub(super) channel_id: ChannelId,
    pub(super) signing_key: Ed25519Key,
    pub(super) node: Node,
    pub(super) config: SequencerConfig,

    // Actor channel for receiving requests from other tasks
    pub(super) request_rx: mpsc::Receiver<ActorRequest>,

    // State
    pub(super) state: Option<TxState>,
    pub(super) current_tip: Option<HeaderId>,
    pub(super) lib_slot: Slot,
    pub(super) last_msg_id: MsgId,
    pub(super) slot_clock: Option<SlotClock>,
    pub(super) channel_state: Option<ChannelState>,
    pub(super) own_key_index: Option<u16>,

    // Block stream
    pub(super) blocks_stream: Option<BoxStream<ProcessedBlockEvent>>,

    // Resubmission
    pub(super) resubmit_interval: tokio::time::Interval,
    pub(super) resubmit_active: bool,
    pub(super) in_flight: FuturesUnordered<BoxFuture<'static, InFlight>>,

    // Buffered events — when multiple events occur on the same block
    pub(super) buffered_events: VecDeque<Event>,

    // Incremental backfill state — processes one batch per next_event() call
    pub(super) backfill_from: Option<Slot>,
    pub(super) backfill_to: Option<Slot>,
    // True on cold start (no checkpoint), false otherwise. Tells
    // `setup_backfill_range` to include the genesis slot in the first
    // backfill range; cleared after that initial range is scheduled so
    // genesis is processed exactly once. A warm restart from a checkpoint
    // with `lib_slot == 0` looks identical without this flag, so we'd
    // otherwise either skip or re-process genesis depending on which side
    // of `+1` we picked.
    pub(super) backfill_from_genesis: bool,

    // Broadcast channel for events — handles subscribe to receive events
    pub(super) event_tx: broadcast::Sender<Event>,

    // Readiness signal — set to true when connected and backfill is complete
    pub(super) ready_tx: watch::Sender<bool>,
    pub(super) channel_view_tx: watch::Sender<SequencerChannelView>,
    pub(super) turn_to_write_tx: watch::Sender<TurnNotification>,
    pub(super) checkpoint_tx: watch::Sender<Option<SequencerCheckpoint>>,
}

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Create a new sequencer with default configuration.
    ///
    /// Returns the sequencer (drive via [`Self::events`]) and a handle (for
    /// submitting requests from other tasks).
    #[must_use]
    pub fn init(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle<Node>) {
        Self::init_with_config(
            channel_id,
            signing_key,
            node,
            SequencerConfig::default(),
            checkpoint,
        )
    }

    /// Create a new sequencer with custom configuration.
    ///
    /// Returns immediately. The sequencer emits [`Event::Readiness`] with
    /// `ready: true` once it has connected and completed backfill.
    ///
    /// Returns the sequencer (drive via [`Self::events`]) and a handle (for
    /// submitting requests from other tasks).
    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle<Node>) {
        let (request_tx, request_rx) = mpsc::channel(config.publish_channel_capacity);

        let (state, lib_slot, last_msg_id, backfill_from_genesis) = if let Some(cp) = checkpoint {
            info!(target: TARGET,
                "Restoring from checkpoint: {} pending txs, lib={:?}, lib_slot={:?}",
                cp.pending_txs.len(),
                cp.lib,
                cp.lib_slot
            );
            let SequencerCheckpoint {
                last_msg_id,
                pending_txs,
                lib,
                lib_slot,
            } = cp;
            let finalized_msg =
                restored_pending_channel_tip(&pending_txs, channel_id).unwrap_or(last_msg_id);
            let mut tx_state = TxState::new(lib, finalized_msg);
            for (_hash, tx) in pending_txs {
                restore_pending_tx(&mut tx_state, tx, channel_id);
            }
            (Some(tx_state), lib_slot, last_msg_id, false)
        } else {
            info!(target: TARGET, "Starting fresh (no checkpoint)");
            (None, Slot::genesis(), MsgId::root(), true)
        };

        let resubmit_interval = tokio::time::interval(config.resubmit_interval);
        let (event_tx, _) = broadcast::channel(256);
        let (ready_tx, ready_rx) = watch::channel(false);
        let (channel_view_tx, _) = watch::channel(SequencerChannelView::new(channel_id));
        let (turn_to_write_tx, _) = watch::channel(TurnNotification {
            our_turn_to_write: false,
            starting_slot: None,
            ends_at_slot: None,
            turn_to_write_slots: None,
            current_slot: None,
        });
        let initial_checkpoint = state
            .as_ref()
            .map(|s| build_checkpoint(s, last_msg_id, lib_slot));
        let (checkpoint_tx, _) = watch::channel(initial_checkpoint);

        let handle = SequencerHandle::new(request_tx, node.clone(), event_tx.clone(), ready_rx);

        let sequencer = Self {
            channel_id,
            signing_key,
            node,
            config,
            request_rx,
            state,
            current_tip: None,
            lib_slot,
            last_msg_id,
            slot_clock: None,
            channel_state: None,
            own_key_index: None,
            blocks_stream: None,
            resubmit_interval,
            resubmit_active: false,
            in_flight: FuturesUnordered::new(),
            buffered_events: VecDeque::new(),
            backfill_from: None,
            backfill_to: None,
            backfill_from_genesis,
            event_tx,
            ready_tx,
            channel_view_tx,
            turn_to_write_tx,
            checkpoint_tx,
        };

        (sequencer, handle)
    }

    /// Whether the sequencer is connected and ready to accept requests.
    ///
    /// Sync snapshot read. For change notifications, use
    /// [`Self::subscribe_ready`].
    #[must_use]
    pub fn is_ready(&self) -> bool {
        *self.ready_tx.borrow()
    }

    /// Current persistence checkpoint, if one has been produced.
    ///
    /// `None` before the first block is processed. Sync snapshot read; for
    /// change notifications, use [`Self::subscribe_checkpoint`].
    #[must_use]
    pub fn checkpoint(&self) -> Option<SequencerCheckpoint> {
        self.checkpoint_tx.borrow().clone()
    }

    /// Subscribe to readiness. Returns a [`watch::Receiver<bool>`] where the
    /// first `.changed().await` returns immediately with the current value;
    /// subsequent calls wait for the next change.
    #[must_use]
    pub fn subscribe_ready(&self) -> watch::Receiver<bool> {
        let mut rx = self.ready_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to channel-view changes. First `.changed().await` returns
    /// immediately with the current [`SequencerChannelView`]; subsequent calls
    /// wait for the next update.
    #[must_use]
    pub fn subscribe_channel_view(&self) -> watch::Receiver<SequencerChannelView> {
        let mut rx = self.channel_view_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to turn-to-write changes. First `.changed().await` returns
    /// immediately with the current [`TurnNotification`]; subsequent calls
    /// wait for the next update.
    #[must_use]
    pub fn subscribe_turn_to_write(&self) -> watch::Receiver<TurnNotification> {
        let mut rx = self.turn_to_write_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to persistence checkpoint changes. First `.changed().await`
    /// returns immediately with the current checkpoint (or `None` if no block
    /// has been processed yet); subsequent calls wait for the next checkpoint
    /// advance (live blocks, backfill batches, state-mutating requests).
    #[must_use]
    pub fn subscribe_checkpoint(&self) -> watch::Receiver<Option<SequencerCheckpoint>> {
        let mut rx = self.checkpoint_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Convert the sequencer into a [`futures::Stream`] of [`Event`]s.
    ///
    /// This is the canonical drive method. The returned stream borrows `self`
    /// mutably for its lifetime, so the typical pattern is to spawn the drive
    /// task and use the [`SequencerHandle`] (returned from [`Self::init`])
    /// for async commands from other tasks. Watch subscriptions
    /// ([`Self::subscribe_ready`], etc.) obtained before the spawn keep
    /// working from anywhere.
    ///
    /// Returns a pinned boxed stream, so callers can use it directly with
    /// [`futures::StreamExt::next`] and [`tokio::select!`] without pinning it
    /// themselves.
    ///
    /// The stream does not terminate on transient sequencer conditions.
    /// Disconnects and recoveries surface as [`Event::Readiness`] transitions.
    pub fn events(&mut self) -> impl futures::Stream<Item = Event> + Send + Unpin + '_ {
        Box::pin(futures::stream::unfold(self, async |seq| {
            // `next_event` can complete a drive step without emitting a public
            // event; loop until we have one. All such non-emitting paths await
            // (network call, sleep, or channel/timer poll), so this loop is
            // not a hot spin.
            loop {
                if let Some(event) = seq.next_event().await {
                    return Some((event, seq));
                }
            }
        }))
    }

    /// Drive the sequencer one step. Returns `None` when the step completed
    /// without emitting a public event (e.g. an empty backfill batch, a
    /// failed reconnect attempt that already slept, a `PrepareTx` /
    /// `SignTx` / `SubmitSignedTx` request handled via oneshot). Internal —
    /// public callers use [`Self::events`], which loops on `None`.
    pub(super) async fn next_event(&mut self) -> Option<Event> {
        // Return buffered event from previous call if any.
        if let Some(event) = self.buffered_events.pop_front() {
            return Some(self.emit_now(event));
        }

        // Process incremental backfill — one batch per call.
        // Returns Some(Some(event)) or Some(None) while active, None when done.
        if let Some(maybe_event) = self.process_incremental_backfill().await {
            return maybe_event.map(|event| self.emit_now(event));
        }

        // Ensure we have a blocks stream (connects if needed).
        if !self.ensure_connected().await {
            return None;
        }

        let stream = self.blocks_stream.as_mut()?;

        tokio::select! {
            // Biased: drain queued publish/sign requests before processing new
            // block events. Prevents a race where a `ChannelUpdate`-triggered
            // republish gets re-orphaned by a fresh block event arriving on
            // the stream before the republish reaches the actor, which could
            // cause duplicate work or duplicate inscriptions.
            biased;
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request)
                    .await
                    .map(|event| self.emit_now(event))
            }
            Some(inflight_result) = self.in_flight.next(), if !self.in_flight.is_empty() => {
                let mut events = self.handle_inflight(inflight_result);
                let first = events.pop_front();
                self.buffered_events.extend(events);
                first.map(|event| self.emit_now(event))
            }
            maybe_event = stream.next() => {
                self.handle_stream_item(maybe_event)
                    .await
                    .map(|event| self.emit_now(event))
            }
            _ = self.resubmit_interval.tick(), if self.current_tip.is_some() && !self.resubmit_active => {
                self.enqueue_pending_submit();
                None
            }
        }
    }

    pub(super) fn emit_now(&self, event: Event) -> Event {
        drop(self.event_tx.send(event.clone()));
        event
    }
}

pub(super) fn build_checkpoint(
    state: &TxState,
    last_msg_id: MsgId,
    lib_slot: Slot,
) -> SequencerCheckpoint {
    SequencerCheckpoint {
        last_msg_id,
        pending_txs: state.all_pending_txs(),
        lib: state.lib(),
        lib_slot,
    }
}

fn restored_pending_channel_tip(
    pending_txs: &[(TxHash, SignedMantleTx)],
    channel_id: ChannelId,
) -> Option<MsgId> {
    let mut parents = Vec::new();
    let mut children = std::collections::HashSet::new();

    for (_, tx) in pending_txs {
        for op in tx.mantle_tx.ops() {
            if let Op::ChannelInscribe(ins) = op
                && ins.channel_id == channel_id
            {
                parents.push(ins.parent);
                children.insert(ins.id());
            }
        }
    }

    parents
        .into_iter()
        .find(|parent| !children.contains(parent))
}

/// Restore a single pending tx into `TxState` on checkpoint resume.
///
/// Inspects the tx ops:
/// - Any `Op::ChannelWithdraw` targeting our channel → bundle. Restored via
///   `submit_atomic_withdraw` so `PendingInscription.withdraws` is repopulated
///   and orphan/finalize emit the correct
///   [`super::types::PublishedTx::AtomicWithdraw`] /
///   [`super::types::OrphanedTx::AtomicWithdraw`] variant.
/// - Only `Op::ChannelInscribe` for our channel → plain inscription.
/// - Neither → treated as opaque (`submit_other`).
///
/// Txs for other channels (checkpoint reused across channels) hit the
/// `submit_other` fallback.
///
/// A tx with 2+ `ChannelInscribe` ops for our channel (constructable via
/// `prepare_tx` + `submit_signed_tx`) isn't a bundle our API can represent.
/// We log an error and fall back to `submit_other` — the tx is still tracked
/// for finalize/orphan, just without per-tx inscription lineage.
pub(super) fn restore_pending_tx(state: &mut TxState, tx: SignedMantleTx, channel_id: ChannelId) {
    let tx_hash = tx.mantle_tx.hash();
    let mut inscribe_meta: Option<(MsgId, MsgId, Inscription)> = None;
    let mut multi_inscribe = false;
    let mut withdraws: Vec<WithdrawInfo> = Vec::new();
    for op in tx.mantle_tx.ops() {
        match op {
            Op::ChannelInscribe(i) if i.channel_id == channel_id => {
                if inscribe_meta.is_some() {
                    multi_inscribe = true;
                } else {
                    inscribe_meta = Some((i.parent, i.id(), i.inscription.clone()));
                }
            }
            Op::ChannelWithdraw(w) if w.channel_id == channel_id => {
                withdraws.push(WithdrawInfo {
                    tx_hash,
                    op: w.clone(),
                });
            }
            _ => {}
        }
    }
    if multi_inscribe {
        tracing::error!(
            target: TARGET,
            tx_hash = %hex::encode(tx.mantle_tx.hash().0),
            "restore_pending_tx: tx has multiple ChannelInscribe ops for our channel; \
             tracking as opaque (no bundle lineage)"
        );
        state.submit_other(tx);
        return;
    }
    match inscribe_meta {
        Some((parent, this_msg, payload)) => {
            if withdraws.is_empty() {
                state.submit_inscription(tx, parent, this_msg, payload);
            } else {
                state.submit_atomic_withdraw(tx, parent, this_msg, payload, withdraws);
            }
        }
        None => state.submit_other(tx),
    }
}
