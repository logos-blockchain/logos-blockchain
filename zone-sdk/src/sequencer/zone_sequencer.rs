use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{
        Op, SignedMantleTx, Transaction as _,
        channel::ChannelState,
        ops::channel::{ChannelId, MsgId, inscribe::Inscription},
        tx::TxHash,
    },
};
use lb_key_management_system_service::keys::Ed25519Key;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

use super::{
    TARGET,
    handle::SequencerHandle,
    slot_clock::SlotClock,
    state::TxState,
    types::{
        Event, SequencerChannelView, SequencerCheckpoint, SequencerConfig, TurnNotification,
        TxSource, TxStatus, TxStatusUpdate, WithdrawInfo,
    },
};
use crate::{adapter, adapter::BoxStream};

/// Zone sequencer.
///
/// The caller drives execution by pumping [`Self::next_event`] or
/// [`Self::events`] in a loop. Publish and admin operations go through a
/// borrowing [`SequencerHandle`] obtained via [`Self::handle`] — the handle's
/// `&mut self` borrow means it can only be used from the drive task, which
/// removes any actor-vs-caller deadlock window and lets every state-mutating
/// method return the resulting [`SequencerCheckpoint`] inline.
pub struct ZoneSequencer<Node> {
    // Config
    pub(super) channel_id: ChannelId,
    pub(super) signing_key: Ed25519Key,
    pub(super) node: Node,
    pub(super) config: SequencerConfig,

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

    // True while the blocks stream is alive AND we have processed at least
    // one event since (re)connecting — i.e. cached `channel_state` and
    // `current_tip` reflect the latest block we observed. Cleared on stream
    // drop; set on each successful `finish_block_processing`. Gates
    // operations that depend on cached on-chain state (inscription turn
    // check, atomic withdraw nonce, channel config) so they fail-fast with
    // `Error::Unavailable` during reconnect rather than building txs from
    // stale state.
    pub(super) connected: bool,

    // Resubmission
    pub(super) resubmit_interval: tokio::time::Interval,

    // In-flight `post_transaction` batches. Publishes and broad sweeps push
    // here; the drive loop polls `in_flight.next()` from `next_event` to
    // drive batches to completion. Each future processes its batch
    // sequentially (one HTTP at a time) and returns one `(tx_hash, success)`
    // per tx. On `success`, `mark_pending_inscription_posted` is called; on
    // failure the tx stays unposted for the next `resubmit_pending` tick.
    pub(super) in_flight: FuturesUnordered<BoxFuture<'static, Vec<(TxHash, bool)>>>,

    // Guard: only one broad sweep in flight at a time. Cleared by the
    // pushed future on completion via the captured `Arc`.
    pub(super) resubmit_active: Arc<AtomicBool>,

    // Tx hashes currently inside a `post_batch` future in `in_flight`.
    // `queue_publish_post` and `queue_resubmit_batch` insert before pushing;
    // the `in_flight` completion handler removes for every batch entry
    // (success or failure). `resubmit_pending` skips entries already in the
    // set so the publish↔resubmit race can't double-post the same tx.
    pub(super) posting: HashSet<TxHash>,

    // Buffered events — when one drive step produces multiple events.
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

    // Broadcast channel for events — late subscribers receive future events.
    pub(super) event_tx: broadcast::Sender<Event>,

    // Readiness latch — set to true once on cold-start backfill completion,
    // never flipped back. Mid-life reconnects don't affect it.
    pub(super) ready_tx: watch::Sender<bool>,
    pub(super) channel_view_tx: watch::Sender<SequencerChannelView>,
    pub(super) turn_to_write_tx: watch::Sender<TurnNotification>,
    pub(super) checkpoint_tx: watch::Sender<Option<SequencerCheckpoint>>,
    pub(super) tx_status_tx: broadcast::Sender<TxStatusUpdate>,
}

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Create a new sequencer with default configuration.
    #[must_use]
    pub fn init(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> Self {
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
    /// Returns immediately. The sequencer emits [`Event::Ready`] once it
    /// has connected and completed cold-start backfill.
    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> Self {
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
            tx_state.prune_local_tx_tracking(config.max_local_tx_tracking);
            (Some(tx_state), lib_slot, last_msg_id, false)
        } else {
            info!(target: TARGET, "Starting fresh (no checkpoint)");
            (None, Slot::genesis(), MsgId::root(), true)
        };

        let resubmit_interval = tokio::time::interval(config.resubmit_interval);
        let (event_tx, _) = broadcast::channel(256);
        let (ready_tx, _ready_rx) = watch::channel(false);
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
        let (tx_status_tx, _) = broadcast::channel(256);

        Self {
            channel_id,
            signing_key,
            node,
            config,
            state,
            current_tip: None,
            lib_slot,
            last_msg_id,
            slot_clock: None,
            channel_state: None,
            own_key_index: None,
            blocks_stream: None,
            connected: false,
            resubmit_interval,
            in_flight: FuturesUnordered::new(),
            resubmit_active: Arc::new(AtomicBool::new(false)),
            posting: HashSet::new(),
            buffered_events: VecDeque::new(),
            backfill_from: None,
            backfill_to: None,
            backfill_from_genesis,
            event_tx,
            ready_tx,
            channel_view_tx,
            turn_to_write_tx,
            checkpoint_tx,
            tx_status_tx,
        }
    }

    /// Obtain a borrowing handle for issuing commands to the sequencer.
    ///
    /// The handle's `&mut self` borrow means only the drive task can hold
    /// one. Methods on the handle mutate state synchronously and return the
    /// resulting [`SequencerCheckpoint`] inline, so the caller can persist
    /// the publish + checkpoint atomically.
    pub const fn handle(&mut self) -> SequencerHandle<'_, Node> {
        SequencerHandle::new(self)
    }

    /// Whether the sequencer has completed cold-start backfill.
    ///
    /// Latched: returns `false` until the first [`Event::Ready`], then `true`
    /// for the rest of the sequencer's lifetime — mid-life reconnects do not
    /// flip this back. Sync snapshot read; for change notifications, use
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

    /// Subscribe to readiness. Returns a [`watch::Receiver<bool>`] that
    /// transitions `false → true` exactly once on cold-start completion and
    /// stays `true`. The first `.changed().await` returns immediately with
    /// the current value (`false` if cold start is still in progress, `true`
    /// once complete); after the latch flips, `.changed().await` never fires
    /// again.
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
    /// advance.
    #[must_use]
    pub fn subscribe_checkpoint(&self) -> watch::Receiver<Option<SequencerCheckpoint>> {
        let mut rx = self.checkpoint_tx.subscribe();
        rx.mark_changed();
        rx
    }

    /// Subscribe to tx-status changes.
    ///
    /// These updates are broadcast as soon as the sequencer classifies a tx.
    /// When a block causes `OnChain`, `Orphaned`, or `Finalized`, the matching
    /// [`super::Event::BlocksProcessed`] is queued separately and may be
    /// observed later by consumers listening to both streams.
    #[must_use]
    pub fn subscribe_tx_status(&self) -> broadcast::Receiver<TxStatusUpdate> {
        self.tx_status_tx.subscribe()
    }

    /// Subscribe to the broadcast channel of events.
    ///
    /// Late subscribers see events emitted from this point on (not the
    /// full history). The primary way to consume events is
    /// [`Self::next_event`] / [`Self::events`] on the drive task; this
    /// broadcast is for additional read-only observers.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Convert the sequencer into a [`futures::Stream`] of [`Event`]s.
    ///
    /// The returned stream borrows `self` mutably for its lifetime. For
    /// drive loops that also need to call [`Self::handle`] in response to
    /// events, prefer [`Self::next_event`] in a `tokio::select!` so the
    /// borrow is released between turns.
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
    /// failed reconnect attempt that already slept, a periodic resubmit
    /// tick). Designed for `tokio::select!` so the drive task can interleave
    /// publish calls via [`Self::handle`] between turns.
    pub async fn next_event(&mut self) -> Option<Event> {
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
            maybe_event = stream.next() => {
                self.handle_stream_item(maybe_event)
                    .await
                    .map(|event| self.emit_now(event))
            }
            _ = self.resubmit_interval.tick(), if self.current_tip.is_some() => {
                self.resubmit_pending();
                None
            }
            Some(results) = self.in_flight.next() => {
                for (tx_hash, success) in results {
                    self.posting.remove(&tx_hash);
                    if success
                        && let Some(state) = self.state.as_mut()
                        && state.mark_pending_inscription_posted(&tx_hash) {
                            self.queue_tx_status(tx_hash, TxStatus::PendingMempool);
                    }
                }
                self.buffered_events.pop_front().map(|event| self.emit_now(event))
            }
        }
    }

    pub(super) fn emit_now(&self, event: Event) -> Event {
        drop(self.event_tx.send(event.clone()));
        event
    }

    pub(super) fn queue_tx_status(&mut self, tx_hash: TxHash, status: TxStatus) {
        let update = TxStatusUpdate { tx_hash, status };
        drop(self.tx_status_tx.send(update));
        if matches!(status, TxStatus::PendingMempool) {
            self.buffered_events
                .push_back(Event::MempoolPending(tx_hash));
        }
        if let Some(state) = self.state.as_mut() {
            match status {
                TxStatus::AcceptedLocally => {
                    state.prune_local_tx_tracking(self.config.max_local_tx_tracking);
                }
                TxStatus::Finalized(TxSource::Local) => {
                    state.remove_local_tx(&tx_hash);
                }
                TxStatus::PendingMempool
                | TxStatus::OnChain(_)
                | TxStatus::Orphaned(_)
                | TxStatus::Finalized(TxSource::Other) => {}
            }
        }
    }

    /// Push a single-tx publish post into `in_flight`. Used by
    /// `handle.publish` / `submit_signed_tx` / `channel_config` —
    /// independent user-initiated actions that can run concurrently.
    pub(super) fn queue_publish_post(&mut self, tx_hash: TxHash, signed_tx: SignedMantleTx) {
        self.posting.insert(tx_hash);
        self.in_flight.push(Box::pin(post_batch(
            self.node.clone(),
            vec![(tx_hash, signed_tx)],
        )));
    }

    /// Push a broad-sweep batch into `in_flight`. Sets `resubmit_active`
    /// before pushing; the future clears it on completion. Caller must
    /// check `resubmit_active` first to avoid duplicate sweeps —
    /// `resubmit_pending` does this gate.
    pub(super) fn queue_resubmit_batch(&mut self, batch: Vec<(TxHash, SignedMantleTx)>) {
        if batch.is_empty() {
            return;
        }
        for (tx_hash, _) in &batch {
            self.posting.insert(*tx_hash);
        }
        self.resubmit_active.store(true, Ordering::Relaxed);
        let node = self.node.clone();
        let flag = Arc::clone(&self.resubmit_active);
        self.in_flight.push(Box::pin(async move {
            let results = post_batch(node, batch).await;
            flag.store(false, Ordering::Relaxed);
            results
        }));
    }
}

async fn post_batch<Node>(node: Node, batch: Vec<(TxHash, SignedMantleTx)>) -> Vec<(TxHash, bool)>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    let mut results = Vec::with_capacity(batch.len());
    let mut iter = batch.into_iter();
    while let Some((tx_hash, signed_tx)) = iter.next() {
        match node.post_transaction(signed_tx).await {
            Ok(()) => results.push((tx_hash, true)),
            Err(e) => {
                // Node looks unavailable — bail on the rest of the batch.
                // Remaining txs stay `!posted` in `state.pending` and the
                // next `resubmit_pending` tick retries.
                warn!(target: TARGET, "Failed to post transaction {tx_hash:?}: {e}; aborting batch");
                results.push((tx_hash, false));
                // Include unattempted txs as failed so the completion
                // handler can clear `posting` for the whole batch and the
                // next `resubmit_pending` picks them up.
                for (tx_hash, _) in iter {
                    results.push((tx_hash, false));
                }
                return results;
            }
        }
    }
    results
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
    let mut children = HashSet::new();

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
///   [`super::types::PendingTx::AtomicWithdraw`] /
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
