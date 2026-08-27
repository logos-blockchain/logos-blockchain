use std::{
    collections::{HashMap, HashSet, VecDeque},
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
        Note, Op, SignedMantleTx, Value,
        channel::{ChannelState, SlotTimeframe, SlotTimeout},
        ledger::{Inputs, Outputs},
        ops::channel::{
            ChannelId, MsgId,
            channel_transfer::ChannelTransferOp,
            config::Keys,
            inscribe::{Inscription, InscriptionOp},
            withdraw::ChannelWithdrawOp,
        },
        traits::Hashable as _,
        transactions::{
            Ops,
            hash::TxHash,
            mantle_tx::{MantleTx as _, RawMantleTx},
            states::Unverified,
        },
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use super::{
    TARGET,
    block_fetch::classify_channel_tx,
    channel_wallet::select_channel_notes,
    client::SequencerClient,
    handle::SequencerHandle,
    slot_clock::SlotClock,
    state::{BlockChannelTx, TxState},
    tx_builder::{
        build_atomic_withdraw_ops_proofs, create_channel_config_tx, create_inscribe_tx,
        find_own_key_index, fund_ops, prepare_tx as build_prepare_tx, sign_tx as build_sign_tx,
    },
    types::{
        AtomicWithdrawInfo, ChannelWalletView, Error, Event, FundingConfig, InscriptionInfo,
        PendingTx, PublishResult, SequencerChannelView, SequencerCheckpoint, SequencerConfig,
        TurnNotification, TxSource, TxStatus, TxStatusUpdate, WithdrawArg, WithdrawInfo,
        WithdrawInputs,
    },
};
use crate::{adapter, adapter::BoxStream};

/// Zone sequencer.
///
/// The caller drives execution by pumping [`Self::next_event`] in a loop.
/// Publish and admin operations go through a
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

    // A block pulled from the stream but not yet converted into a public
    // event. Keeping it on the sequencer makes `next_event` cancellation-safe:
    // if its future is dropped while block processing awaits node data, the
    // next call resumes the same block instead of pulling past it.
    pub(super) pending_block_event: Option<Arc<ProcessedBlockEvent>>,

    // True while the blocks stream is alive AND we have processed at least
    // one event since (re)connecting — i.e. cached `channel_state` and
    // `current_tip` reflect the latest block we observed. Cleared on stream
    // drop; set on each successful `finish_block_processing`. Gates
    // operations that depend on cached on-chain state (inscription turn
    // check, atomic withdraw nonce, channel config) so they fail-fast with
    // `Error::Unavailable` during reconnect rather than building txs from
    // stale state. It also gates every publish-type operation (funding needs
    // the node); a fresh `Event::Ready` is emitted when the reconnect
    // completes.
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

    // Request channel for actor-routed commands from cheap-to-clone
    // `SequencerClient`s. `request_tx` is retained so `client()` can vend new
    // senders; `request_rx` is drained inside `next_event`.
    request_tx: mpsc::UnboundedSender<ActorRequest>,
    request_rx: mpsc::UnboundedReceiver<ActorRequest>,
}

pub type PublishReceipt = (PublishResult, SequencerCheckpoint);

/// Internal request enum routed through the actor's `request_rx` channel.
///
/// Sent by [`SequencerClient`] from any task; processed inside
/// [`ZoneSequencer::next_event`] and replied to via the embedded oneshot.
/// One variant per [`SequencerHandle`] method, so the client surface is a
/// 1:1 async mirror of the drive-loop handle.
pub(super) enum ActorRequest {
    Publish {
        data: Inscription,
        response_tx: oneshot::Sender<Result<PublishReceipt, Error>>,
    },
    PublishAtomicWithdraw {
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
        inputs: WithdrawInputs,
        response_tx: oneshot::Sender<Result<PublishReceipt, Error>>,
    },
    ChannelConfig {
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        transfer_threshold: u16,
        response_tx: oneshot::Sender<Result<(PublishReceipt, SignedMantleTx<Unverified>), Error>>,
    },
    SubmitSignedTx {
        tx: SignedMantleTx<Unverified>,
        msg_id: MsgId,
        response_tx: oneshot::Sender<Result<PublishReceipt, Error>>,
    },
    PrepareTx {
        ops: Ops,
        data: Inscription,
        response_tx: oneshot::Sender<Result<(RawMantleTx, MsgId, Ed25519Signature), Error>>,
    },
    SignTx {
        tx: RawMantleTx,
        response_tx: oneshot::Sender<Result<Ed25519Signature, Error>>,
    },
    ChannelWallet {
        response_tx: oneshot::Sender<ChannelWalletView>,
    },
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
        funding: FundingConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> Self {
        Self::init_with_config(
            channel_id,
            signing_key,
            node,
            SequencerConfig::new(funding),
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
                channel_notes,
                finalized_config,
            } = cp;
            let finalized_msg =
                restored_pending_channel_tip(&pending_txs, channel_id).unwrap_or(last_msg_id);
            let mut tx_state = TxState::new(lib, finalized_msg);
            tx_state.set_finalized_config(finalized_config);
            tx_state.restore_channel_notes(channel_notes);
            for (_hash, tx) in pending_txs {
                track_pending_tx(&mut tx_state, tx, channel_id);
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
        let (request_tx, request_rx) = mpsc::unbounded_channel();

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
            pending_block_event: None,
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
            request_tx,
            request_rx,
        }
    }

    /// Obtain a borrowing handle for issuing commands to the sequencer.
    ///
    /// The handle's `&mut self` borrow means only the drive task can hold
    /// one. Methods on the handle mutate state directly on the drive task
    /// and return the resulting [`SequencerCheckpoint`] inline, so the
    /// caller can persist the publish + checkpoint atomically. Publish-type
    /// methods await one funding round-trip first.
    pub const fn handle(&mut self) -> SequencerHandle<'_, Node> {
        SequencerHandle::new(self)
    }

    /// Obtain a cheap-to-clone client for driving the sequencer from tasks
    /// outside the drive loop.
    ///
    /// Unlike [`Self::handle`], the client does not borrow the sequencer —
    /// publish/admin commands are routed through an internal channel and
    /// processed inside [`Self::next_event`], and watch/broadcast
    /// subscriptions are vended directly from cloned senders. The drive loop
    /// must therefore still be polled for client publish calls to make
    /// progress; an unpolled sequencer will leave client awaits pending
    /// forever. Subscriptions, on the other hand, work regardless of whether
    /// the drive loop is being polled (they're just receivers attached to the
    /// senders the actor updates).
    #[must_use]
    pub fn client(&self) -> SequencerClient {
        SequencerClient::new(
            self.request_tx.clone(),
            self.event_tx.clone(),
            self.ready_tx.clone(),
            self.channel_view_tx.clone(),
            self.turn_to_write_tx.clone(),
            self.checkpoint_tx.clone(),
            self.tx_status_tx.clone(),
        )
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

    /// The channel's note set as tracked from block data: finalized base
    /// plus the unfinalized notes on the currently tracked branch. Empty
    /// until the first block is processed.
    #[must_use]
    pub fn channel_wallet(&self) -> ChannelWalletView {
        self.state
            .as_ref()
            .map(|s| s.channel_wallet_view(self.current_tip))
            .unwrap_or_default()
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
    /// [`Self::next_event`] on the drive task; this broadcast is for
    /// additional read-only observers.
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Drive the sequencer until the next public event.
    ///
    /// Loops internally over drive steps that complete work without emitting
    /// (in-progress backfill batches, periodic resubmit ticks, in-flight post
    /// completions, reconnect retries), so the caller's loop body always
    /// receives a real [`Event`] — no `Option` unwrapping required.
    ///
    /// # Block-event cancellation safety
    ///
    /// Cancelling this future does not lose or partially apply a block event.
    /// A pulled block is retained until its event is returned, and all fallible
    /// node reads complete before the corresponding state mutation.
    ///
    /// A [`SequencerClient`](super::SequencerClient) command selected from the
    /// request queue may instead fail with [`Error::Unavailable`] if this
    /// future is cancelled while handling it. The client can retry that
    /// command.
    pub async fn next_event(&mut self) -> Event {
        loop {
            if let Some(ev) = self.step().await {
                return ev;
            }
        }
    }

    /// Drive the sequencer one step. Returns `None` when the step completed
    /// without emitting a public event (e.g. an empty backfill batch, a
    /// failed reconnect attempt that already slept, a periodic resubmit
    /// tick). Internal: callers use [`Self::next_event`] which loops until
    /// an event is produced.
    async fn step(&mut self) -> Option<Event> {
        // Return buffered event from previous call if any.
        if let Some(event) = self.buffered_events.pop_front() {
            return Some(self.emit_now(event));
        }

        // Finish a block already pulled from the stream before doing any
        // other drive work. `process_pending_block_event` only clears it once
        // processing has produced the corresponding public event.
        if self.pending_block_event.is_some() {
            return self.process_pending_block_event().await;
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
                let Some(block_event) = maybe_event else {
                    self.handle_stream_disconnect();
                    return None;
                };

                self.pending_block_event = Some(Arc::new(block_event));
                None
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
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request).await;
                None
            }
        }
    }

    async fn handle_request(&mut self, request: ActorRequest) {
        match request {
            ActorRequest::Publish { data, response_tx } => {
                drop(response_tx.send(self.do_publish(data).await));
            }
            ActorRequest::PublishAtomicWithdraw {
                inscribe,
                withdraws,
                inputs,
                response_tx,
            } => {
                drop(
                    response_tx.send(
                        self.do_publish_atomic_withdraw(inscribe, withdraws, inputs)
                            .await,
                    ),
                );
            }
            ActorRequest::ChannelConfig {
                keys,
                posting_timeframe,
                posting_timeout,
                configuration_threshold,
                transfer_threshold,
                response_tx,
            } => {
                drop(
                    response_tx.send(
                        self.do_channel_config(
                            keys,
                            posting_timeframe,
                            posting_timeout,
                            configuration_threshold,
                            transfer_threshold,
                        )
                        .await,
                    ),
                );
            }
            ActorRequest::SubmitSignedTx {
                tx,
                msg_id,
                response_tx,
            } => {
                drop(response_tx.send(self.do_submit_signed_tx(tx, msg_id)));
            }
            ActorRequest::ChannelWallet { response_tx } => {
                drop(response_tx.send(self.channel_wallet()));
            }
            ActorRequest::PrepareTx {
                ops,
                data,
                response_tx,
            } => {
                drop(response_tx.send(self.do_prepare_tx(ops, data)));
            }
            ActorRequest::SignTx { tx, response_tx } => {
                drop(response_tx.send(self.do_sign_tx(&tx)));
            }
        }
    }

    /// Wait the configured reconnect delay, but keep servicing client
    /// [`ActorRequest`]s while waiting.
    ///
    /// This preserves the same post-[`Event::Ready`] local-acceptance
    /// semantics as
    /// [`SequencerHandle::publish`](super::SequencerHandle::publish):
    /// a publish that arrives via [`SequencerClient`](super::SequencerClient)
    /// during a reconnect is handled immediately (queued locally via
    /// `do_publish`) instead of blocking until connectivity is restored.
    /// Without this, client requests would sit unserviced until
    /// [`Self::ensure_connected`] succeeds, since `request_rx` is otherwise
    /// only drained from `step`'s `select!` after connection.
    ///
    /// The sleep is pinned so the backoff keeps elapsing across iterations: any
    /// number of requests can be serviced during the wait without resetting or
    /// short-circuiting the delay.
    pub(super) async fn wait_reconnect_delay(&mut self) {
        let sleep = tokio::time::sleep(self.config.reconnect_delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                Some(request) = self.request_rx.recv() => self.handle_request(request).await,
            }
        }
    }

    /// Building a transaction requires a round-trip to the node's wallet —
    /// fail fast with [`Error::Unavailable`] while disconnected instead of
    /// surfacing an HTTP error from the fund call. A fresh [`Event::Ready`]
    /// is emitted once the reconnect completes, so callers have a positive
    /// signal to retry.
    const fn ensure_fundable(&self) -> Result<(), Error> {
        if !self.connected {
            return Err(Error::Unavailable {
                reason: "node disconnected; funding a transaction requires a connected node",
            });
        }
        Ok(())
    }

    fn ensure_ready(&self) -> Result<(), Error> {
        if !self.is_ready() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        if self.state.is_none() {
            return Err(Error::Unavailable {
                reason: "sequencer state not initialized",
            });
        }
        Ok(())
    }

    /// Core publish logic. Shared by [`SequencerHandle::publish`] (called
    /// from the drive task) and the actor's [`ActorRequest::Publish`] handler
    /// (called from outside the drive task via [`SequencerClient`]).
    pub(super) async fn do_publish(&mut self, data: Inscription) -> Result<PublishReceipt, Error> {
        self.ensure_ready()?;
        self.ensure_fundable()?;

        let parent = self.compute_publish_parent();
        let (signed_tx, new_msg_id) = create_inscribe_tx(
            &self.node,
            &self.config.funding,
            self.channel_id,
            &self.signing_key,
            data.clone(),
            parent,
        )
        .await?;
        let id = signed_tx.mantle_tx().hash();

        debug!(target: TARGET,
            "Prepared publish: payload={:?}, parent={}, msg_id={}, tx={}",
            String::from_utf8_lossy(&data),
            hex::encode(parent.as_ref()),
            hex::encode(new_msg_id.as_ref()),
            hex::encode(id.0),
        );

        let info = InscriptionInfo {
            tx_hash: id,
            parent_msg: parent,
            this_msg: new_msg_id,
            payload: data.clone(),
            signer: Some(self.signing_key.public_key()),
        };

        // Safe to unwrap — `ensure_ready` checks state.
        let state = self.state.as_mut().unwrap();
        state.submit_inscription(signed_tx.clone(), parent, new_msg_id, data);
        self.last_msg_id = new_msg_id;
        self.queue_tx_status(id, TxStatus::AcceptedLocally);

        if self.can_publish_inscription_now() {
            self.queue_publish_post(id, signed_tx);
        }

        self.publish_channel_view();

        let checkpoint = self.publish_checkpoint().ok_or(Error::Unavailable {
            reason: "checkpoint unavailable",
        })?;

        Ok((
            PublishResult {
                tx: PendingTx::Inscription(info),
            },
            checkpoint,
        ))
    }

    /// Core atomic-withdraw logic. Builds the client-side bundle
    /// `[CHANNEL_INSCRIBE, CHANNEL_TRANSFER, CHANNEL_WITHDRAW]`: the transfer
    /// moves channel notes to the recipient keys (plus a change note back to
    /// the sequencer's own key when the selected inputs overpay), and the
    /// withdraw releases exactly the freshly-created recipient notes. A channel
    /// withdraw only releases an existing channel note to the key it already
    /// carries, so paying a fresh recipient requires transferring a note to
    /// that key first — which is why this depends on the sequencer tracking the
    /// channel's notes.
    ///
    /// Mirrors [`Self::do_publish`] for the readiness/funding checks, parent
    /// computation, status queueing and checkpointing. Scoped to single-signer
    /// (centralized) channels — only the sequencer's own signature proves the
    /// transfer and withdraw ops.
    pub(super) async fn do_publish_atomic_withdraw(
        &mut self,
        inscribe: Inscription,
        withdraws: Vec<WithdrawArg>,
        inputs: WithdrawInputs,
    ) -> Result<PublishReceipt, Error> {
        self.ensure_ready()?;
        self.ensure_fundable()?;

        if withdraws.is_empty() {
            return Err(Error::Network(
                "publish_atomic_withdraw requires at least one withdraw".into(),
            ));
        }

        // Use the cached channel state kept fresh by the drive loop — see
        // `ensure_connected` for the staleness gate.
        let channel_state = self.channel_state.as_ref().ok_or_else(|| {
            Error::Network(format!(
                "publish_atomic_withdraw requires channel state for {:?}",
                self.channel_id
            ))
        })?;
        if channel_state.transfer_threshold > 1 {
            return Err(Error::Network(format!(
                "publish_atomic_withdraw requires transfer_threshold == 1, got {}",
                channel_state.transfer_threshold
            )));
        }
        let own_key_index = find_own_key_index(channel_state, &self.signing_key)?;

        // Recipient notes, concatenated across the withdraw args in order, plus
        // the total value they release.
        let recipient_outputs: Vec<Note> = withdraws
            .iter()
            .flat_map(|w| w.outputs.iter().copied())
            .collect();
        let amount = recipient_outputs.iter().try_fold(0u64, |acc, note| {
            acc.checked_add(note.value)
                .ok_or_else(|| Error::Network("withdraw amount overflow".into()))
        })?;

        let (transfer_op, withdraw_op) =
            self.build_transfer_and_withdraw(&recipient_outputs, amount, &inputs)?;

        let parent = self.compute_publish_parent();
        let inscription_op = InscriptionOp {
            channel_id: self.channel_id,
            inscription: inscribe.clone(),
            parent,
            signer: self.signing_key.public_key(),
        };
        let msg_id = inscription_op.id();

        let ops = vec![
            Op::ChannelInscribe(inscription_op),
            Op::ChannelTransfer(transfer_op),
            Op::ChannelWithdraw(withdraw_op.clone()),
        ];

        let (tx, transfer_proof) = fund_ops(&self.node, &self.config.funding, ops).await?;
        let own_sig = build_sign_tx(tx.hash(), &self.signing_key);
        let ops_proofs =
            build_atomic_withdraw_ops_proofs(&tx, own_key_index, own_sig, transfer_proof.as_ref())?;
        let signed_tx = SignedMantleTx::new(tx, ops_proofs);

        let tx_hash = signed_tx.mantle_tx().hash();
        let withdraw_infos = vec![WithdrawInfo {
            tx_hash,
            op: withdraw_op,
        }];

        debug!(target: TARGET,
            "Prepared atomic withdraw: payload={:?}, parent={}, msg_id={}, tx={}, amount={}",
            String::from_utf8_lossy(&inscribe),
            hex::encode(parent.as_ref()),
            hex::encode(msg_id.as_ref()),
            hex::encode(tx_hash.0),
            amount,
        );

        // Safe to unwrap — `ensure_ready` checks state.
        let state = self.state.as_mut().unwrap();
        state.submit_atomic_withdraw(
            signed_tx.clone(),
            parent,
            msg_id,
            inscribe.clone(),
            withdraw_infos.clone(),
        );
        self.last_msg_id = msg_id;
        self.queue_tx_status(tx_hash, TxStatus::AcceptedLocally);

        if self.can_publish_inscription_now() {
            self.queue_publish_post(tx_hash, signed_tx);
        }

        self.publish_channel_view();

        let checkpoint = self.publish_checkpoint().ok_or(Error::Unavailable {
            reason: "checkpoint unavailable",
        })?;

        Ok((
            PublishResult {
                tx: PendingTx::AtomicWithdraw(AtomicWithdrawInfo {
                    tx_hash,
                    inscription: InscriptionInfo {
                        tx_hash,
                        parent_msg: parent,
                        this_msg: msg_id,
                        payload: inscribe,
                        signer: Some(self.signing_key.public_key()),
                    },
                    withdraws: withdraw_infos,
                }),
            },
            checkpoint,
        ))
    }

    /// Build the transfer + withdraw ops for an atomic withdraw: select the
    /// channel notes that cover `amount`, transfer them to the recipient keys
    /// (with change back to the funding key when they overpay), and release the
    /// freshly-created recipient notes.
    fn build_transfer_and_withdraw(
        &self,
        recipient_outputs: &[Note],
        amount: Value,
        inputs: &WithdrawInputs,
    ) -> Result<(ChannelTransferOp, ChannelWithdrawOp), Error> {
        let funding_pk = self.config.funding.funding_pk;
        let view = self.channel_wallet();
        let selected = select_channel_notes(&view, funding_pk, amount, inputs)?;
        let by_id: HashMap<_, _> = view
            .finalized
            .iter()
            .chain(view.unfinalized.iter())
            .map(|n| (n.note_id, n.value))
            .collect();
        let selected_sum = selected.iter().try_fold(0u64, |acc, id| {
            let value = by_id.get(id).ok_or_else(|| {
                Error::Network("selected channel note vanished from the tracked set".into())
            })?;
            acc.checked_add(*value)
                .ok_or_else(|| Error::Network("selected channel note value overflow".into()))
        })?;
        let change = selected_sum.checked_sub(amount).ok_or_else(|| {
            Error::Network("selected channel notes underfund the withdrawal".into())
        })?;

        // Transfer outputs: the recipient notes, then a change note when the
        // inputs overpay. Change routes to `change_pk` if configured, else back
        // to the funding key.
        let change_pk = self.config.funding.change_pk.unwrap_or(funding_pk);
        let mut transfer_outputs = recipient_outputs.to_vec();
        if change > 0 {
            transfer_outputs.push(Note::new(change, change_pk));
        }
        let transfer_op = ChannelTransferOp {
            channel_id: self.channel_id,
            inputs: Inputs::try_new(selected)
                .map_err(|e| Error::Network(format!("invalid transfer inputs: {e:?}")))?,
            outputs: Outputs::try_new(transfer_outputs)
                .map_err(|e| Error::Network(format!("invalid transfer outputs: {e:?}")))?,
        };

        // The withdraw releases exactly the recipient notes the transfer
        // created: the first `recipient_outputs.len()` utxos in output order. A
        // trailing change note, if any, stays in the channel.
        let recipient_note_ids: Vec<_> = transfer_op
            .utxos()
            .take(recipient_outputs.len())
            .map(|utxo| utxo.id())
            .collect();
        let withdraw_op = ChannelWithdrawOp {
            channel_id: self.channel_id,
            inputs: Inputs::try_new(recipient_note_ids)
                .map_err(|e| Error::Network(format!("invalid withdraw inputs: {e:?}")))?,
        };

        Ok((transfer_op, withdraw_op))
    }

    pub(super) async fn do_channel_config(
        &mut self,
        keys: Keys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: u16,
        transfer_threshold: u16,
    ) -> Result<(PublishReceipt, SignedMantleTx<Unverified>), Error> {
        self.ensure_ready()?;
        self.ensure_fundable()?;

        // Per the Mantle spec, configuring an unclaimed channel requires no
        // signatures — validation skips the signature check entirely — so
        // claim with an empty proof. This also keeps the node wallet's fee
        // prediction exact: it predicts a threshold-0 multi-sig proof for a
        // channel it cannot see yet, and a superfluous signature would make
        // the funded fee undershoot the actual storage cost.
        //
        // For an existing channel the signature must claim our key's index in
        // the *current* accredited list — that is what the ledger verifies
        // against. Signature collection for `configuration_threshold > 1` is
        // out of scope, so reject it early instead of submitting a
        // transaction that can only die at block assembly.
        let signer = match &self.channel_state {
            None => None,
            Some(channel) => {
                if channel.configuration_threshold != 1 {
                    return Err(Error::Network(format!(
                        "channel config update needs {} signatures; this one-shot \
                         helper only supports single-signer channels \
                         (configuration_threshold 1) — collect the signatures \
                         out-of-band and submit the fully-signed transaction via \
                         `submit_signed_tx` instead",
                        channel.configuration_threshold
                    )));
                }
                let index = self.own_key_index.ok_or_else(|| {
                    Error::Network("sequencer key not in channel accredited_keys".into())
                })?;
                Some((index, &self.signing_key))
            }
        };

        // The configuration extends our *local* config tip — the same view the
        // config shed evaluates against — so a config we submit is never
        // transiently orphaned by an ahead-of-us node config tip. If we are
        // behind the node the parent is stale, so the config simply does not
        // land (parent mismatch) and is orphaned and resubmitted once we catch
        // up — the same self-healing path an inscription takes. Single-signer
        // only, so we always sign with our own key; multi-sig configs are
        // rejected above and go through the consumer's `submit_signed_tx` flow.
        // We chain on the mined tip, not a config of ours still in flight: a
        // reconfiguration lands one at a time.
        let parent = match (self.state.as_ref(), self.current_tip) {
            (Some(state), Some(tip)) => state.config_tip_at(tip),
            _ => MsgId::root(),
        };

        let signed_tx = create_channel_config_tx(
            &self.node,
            &self.config.funding,
            self.channel_id,
            parent,
            signer,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        )
        .await?;
        let tx_hash = signed_tx.mantle_tx().hash();

        // Safe to unwrap — `ensure_ready` checks state.
        let state = self.state.as_mut().unwrap();
        state.submit_other(signed_tx.clone(), self.channel_id);
        self.queue_tx_status(tx_hash, TxStatus::AcceptedLocally);

        info!(target: TARGET, "Submitted channel_config transaction {}", hex::encode(tx_hash.0));

        self.queue_publish_post(tx_hash, signed_tx.clone());

        self.publish_channel_view();

        let checkpoint = self.publish_checkpoint().ok_or(Error::Unavailable {
            reason: "checkpoint unavailable",
        })?;

        let publish_result = PublishResult {
            tx: PendingTx::Inscription(InscriptionInfo {
                tx_hash,
                parent_msg: self.last_msg_id,
                this_msg: self.last_msg_id,
                payload: Inscription::new_unchecked(Vec::new()),
                // Config entry: authorized by a key threshold, no single author.
                signer: None,
            }),
        };
        let publish_receipt = (publish_result, checkpoint);
        Ok((publish_receipt, signed_tx))
    }

    pub(super) fn do_submit_signed_tx(
        &mut self,
        tx: SignedMantleTx<Unverified>,
        msg_id: MsgId,
    ) -> Result<PublishReceipt, Error> {
        self.ensure_ready()?;

        // Safe to unwrap — `ensure_ready` checks state.
        let state = self.state.as_mut().unwrap();
        let id = tx.mantle_tx().hash();
        let derived_tip = track_pending_tx(state, tx.clone(), self.channel_id);
        let parent_msg = self.last_msg_id;
        // The tip the tx leaves behind is defined by its inscriptions (the
        // last one); a tx without any — e.g. a pure config — leaves the tip
        // unchanged and has no tip for the caller's `msg_id` to match, so
        // only a mis-declared inscription tip is worth warning about.
        let new_tip = derived_tip.unwrap_or(self.last_msg_id);
        if derived_tip.is_some_and(|derived| derived != msg_id) {
            warn!(target: TARGET,
                "submit_signed_tx: caller msg_id {:?} is not the tx's resulting channel tip {:?}; \
                 using the derived tip",
                msg_id, new_tip
            );
        }
        self.last_msg_id = new_tip;
        self.queue_tx_status(id, TxStatus::AcceptedLocally);

        info!(target: TARGET, "Submitted tx including inscription {:?}", id);

        let (payload, signer) = tx
            .mantle_tx()
            .ops()
            .iter()
            .find_map(|op| match op {
                Op::ChannelInscribe(i) if i.channel_id == self.channel_id => {
                    Some((i.inscription.clone(), Some(i.signer)))
                }
                _ => None,
            })
            .unwrap_or_else(|| (Inscription::new_unchecked(Vec::new()), None));

        self.queue_publish_post(id, tx);

        let checkpoint = self.publish_checkpoint().ok_or(Error::Unavailable {
            reason: "checkpoint unavailable",
        })?;

        Ok((
            PublishResult {
                tx: PendingTx::Inscription(InscriptionInfo {
                    tx_hash: id,
                    parent_msg,
                    this_msg: new_tip,
                    payload,
                    signer,
                }),
            },
            checkpoint,
        ))
    }

    pub(super) fn do_prepare_tx(
        &self,
        ops: Ops,
        data: Inscription,
    ) -> Result<(RawMantleTx, MsgId, Ed25519Signature), Error> {
        self.ensure_ready()?;
        let parent = self.compute_publish_parent();
        Ok(build_prepare_tx(
            ops,
            self.channel_id,
            &self.signing_key,
            data,
            parent,
        ))
    }

    pub(super) fn do_sign_tx(&self, tx: &RawMantleTx) -> Result<Ed25519Signature, Error> {
        self.ensure_ready()?;
        Ok(build_sign_tx(tx.hash(), &self.signing_key))
    }

    pub(super) fn compute_publish_parent(&self) -> MsgId {
        // Safe to unwrap — callers gate on `state.is_some()`.
        let state = self.state.as_ref().unwrap();
        self.current_tip
            .map_or(self.last_msg_id, |tip| state.publish_parent(tip))
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
    pub(super) fn queue_publish_post(
        &mut self,
        tx_hash: TxHash,
        signed_tx: SignedMantleTx<Unverified>,
    ) {
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
    pub(super) fn queue_resubmit_batch(
        &mut self,
        batch: Vec<(TxHash, SignedMantleTx<Unverified>)>,
    ) {
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

async fn post_batch<Node>(
    node: Node,
    batch: Vec<(TxHash, SignedMantleTx<Unverified>)>,
) -> Vec<(TxHash, bool)>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    let mut results = Vec::with_capacity(batch.len());
    let mut iter = batch.into_iter();
    while let Some((tx_hash, signed_tx)) = iter.next() {
        match node.post_transaction(signed_tx).await {
            Ok(()) => results.push((tx_hash, true)),
            Err(e) => {
                // TODO: We need to distinguish node rejection (invalid tx) from node
                // unavailability.   Any post error (e.g. network failure, badly
                // formed tx) lands here: the tx stays   pending, is retried
                // every time, and is only ever evicted by chain inclusion.
                //   That never happens for a rejected tx.
                //   Since batches are parent-before-child and we abort on first error, one
                // rejected   inscription also blocks every tx behind it.
                //   Rejections should evict the tx.

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
        channel_notes: state.channel_notes_base(),
        finalized_config: state.finalized_config(),
    }
}

fn restored_pending_channel_tip(
    pending_txs: &[(TxHash, SignedMantleTx<Unverified>)],
    channel_id: ChannelId,
) -> Option<MsgId> {
    let mut parents = Vec::new();
    let mut children = HashSet::new();

    for (_, tx) in pending_txs {
        for op in tx.mantle_tx().ops() {
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

/// Track a signed tx in pending state: publish-shaped txs enter the
/// inscription lineage, everything else is tracked opaquely.
/// Returns the channel tip the tx leaves behind once mined (its last
/// tip-advancing op), or `None` when the tx carries none for this channel.
pub(super) fn track_pending_tx(
    state: &mut TxState,
    tx: SignedMantleTx<Unverified>,
    channel_id: ChannelId,
) -> Option<MsgId> {
    match classify_channel_tx(&tx, channel_id, &mut None) {
        Some(BlockChannelTx::Inscription(i)) => {
            let this_msg = i.this_msg;
            state.submit_inscription(tx, i.parent_msg, this_msg, i.payload);
            Some(this_msg)
        }
        Some(BlockChannelTx::AtomicWithdraw(aw)) => {
            let this_msg = aw.inscription.this_msg;
            state.submit_atomic_withdraw(
                tx,
                aw.inscription.parent_msg,
                this_msg,
                aw.inscription.payload,
                aw.withdraws,
            );
            Some(this_msg)
        }
        _ => state.submit_other(tx, channel_id),
    }
}
