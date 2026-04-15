use std::{pin::Pin, time::Duration};

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{ProcessedBlockEvent, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        ops::{
            Op, OpProof,
            channel::{
                ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp, set_keys::SetKeysOp,
            },
        },
        tx::TxHash,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519Signature};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::{
    adapter,
    state::{InscriptionInfo, TxState},
};

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;
const BACKFILL_BATCH_SIZE: u64 = 100;

/// Inscription identifier.
pub type InscriptionId = TxHash;

/// Checkpoint for stop/resume functionality.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SequencerCheckpoint {
    /// Last message ID for chain continuity.
    pub last_msg_id: MsgId,
    /// Pending transactions to restore.
    pub pending_txs: Vec<(TxHash, SignedMantleTx)>,
    /// Last known LIB.
    pub lib: HeaderId,
    /// Last known LIB slot (for backfill range queries).
    pub lib_slot: Slot,
}

/// Result of a publish operation.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// The inscription ID (transaction hash).
    pub inscription_id: InscriptionId,
    /// Current checkpoint for persistence.
    pub checkpoint: SequencerCheckpoint,
}

/// Configuration for the zone sequencer.
#[derive(Clone)]
pub struct SequencerConfig {
    pub resubmit_interval: Duration,
    pub reconnect_delay: Duration,
    pub publish_channel_capacity: usize,
}

impl Default for SequencerConfig {
    fn default() -> Self {
        Self {
            resubmit_interval: DEFAULT_RESUBMIT_INTERVAL,
            reconnect_delay: DEFAULT_RECONNECT_DELAY,
            publish_channel_capacity: DEFAULT_PUBLISH_CHANNEL_CAPACITY,
        }
    }
}

/// Sequencer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sequencer unavailable: {reason}")]
    Unavailable { reason: &'static str },
    #[error("network error: {0}")]
    Network(String),
}

/// Events emitted by the sequencer.
#[derive(Debug, Clone)]
pub enum Event {
    /// Transactions finalized (at or below LIB).
    TxsFinalized {
        tx_hashes: Vec<TxHash>,
        inscriptions: Vec<InscriptionInfo>,
    },
    /// Channel state changed.
    ///
    /// When `invalidated` is empty, this is a simple extension — new
    /// inscriptions appeared without conflicting with our pending chain.
    /// When `invalidated` is non-empty, a competing inscription or L1 reorg
    /// invalidated some of our pending inscriptions.
    ChannelUpdate {
        /// Our pending inscriptions that are now invalid (parent taken).
        invalidated: Vec<InscriptionInfo>,
        /// New inscriptions that appeared on chain since the last common
        /// message.
        adopted: Vec<InscriptionInfo>,
        /// The new channel tip `MsgId`.
        new_channel_tip: MsgId,
    },
    /// Batch of finalized inscriptions discovered during backfill catch-up.
    /// Emitted incrementally when the sequencer catches up from a checkpoint.
    FinalizedInscriptions { inscriptions: Vec<InscriptionInfo> },
    /// Sequencer is connected, backfill complete, ready to accept publishes.
    Ready,
    /// An inscription was created and submitted to the network.
    Published {
        inscription_id: InscriptionId,
        checkpoint: SequencerCheckpoint,
    },
}

enum ActorRequest {
    /// Create/sign/submit a transaction with an inscription
    PublishMessage { data: Vec<u8> },
    /// Build an unsigned tx for the given ops and an inscription
    ///
    /// Calling this multiple times without submitting the prepared txs via
    /// `SubmitSignedTx` can cause parent msg ID conflicts, so ensure
    /// prepared txs are submitted promptly. If additional prepares are
    /// unavoidable, handle potential conflicts carefully.
    PrepareTx {
        ops: Vec<Op>,
        msg: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(MantleTx, MsgId, Ed25519Signature), Error>>,
    },
    /// Submit a signed tx associated with a msg ID
    SubmitSignedTx {
        tx: SignedMantleTx,
        msg_id: MsgId,
        reply: tokio::sync::oneshot::Sender<Result<PublishResult, Error>>,
    },
    SetKeys {
        keys: Vec<Ed25519PublicKey>,
        reply: tokio::sync::oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
}

enum InFlight {
    ResubmittedBatch {
        results: Vec<(InscriptionId, Result<(), String>)>,
    },
}

/// Handle for submitting requests to the sequencer from other tasks.
///
/// This is cheaply cloneable and can be shared across tasks.
#[derive(Clone)]
pub struct SequencerHandle<Node> {
    request_tx: mpsc::Sender<ActorRequest>,
    node: Node,
    event_tx: broadcast::Sender<Event>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
}

impl<Node> SequencerHandle<Node>
where
    Node: adapter::Node + Sync,
{
    /// Subscribe to sequencer events.
    ///
    /// Use this with [`spawn`](ZoneSequencer::spawn) to react to events
    /// without driving the event loop manually:
    ///
    /// ```ignore
    /// let (sequencer, handle) = ZoneSequencer::init(channel_id, key, url, None, None);
    /// sequencer.spawn();
    /// let mut events = handle.subscribe();
    /// while let Ok(event) = events.recv().await {
    ///     // handle event
    /// }
    /// ```
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.event_tx.subscribe()
    }

    /// Wait until the sequencer is connected and ready to accept requests.
    pub async fn wait_ready(&mut self) {
        while !*self.ready_rx.borrow_and_update() {
            if self.ready_rx.changed().await.is_err() {
                return; // sequencer dropped
            }
        }
    }

    /// Publish an inscription to the zone's channel.
    ///
    /// Fire-and-forget: the inscription is queued for processing by the
    /// sequencer's event loop. The result (inscription ID + checkpoint) is
    /// delivered via [`Event::Published`] once the tx is created and posted
    /// to the network.
    pub async fn publish_message(&self, data: Vec<u8>) -> Result<(), Error> {
        if !*self.ready_rx.borrow() {
            return Err(Error::Unavailable {
                reason: "sequencer not yet ready",
            });
        }
        self.request_tx
            .send(ActorRequest::PublishMessage { data })
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })
    }

    /// Build a [`MantleTx`] for the given ops and an inscription message,
    /// without submitting it.
    ///
    /// The returned [`MantleTx`] should be signed by all parties and submitted
    /// via [`Self::submit_signed_tx`].
    pub async fn prepare_tx(
        &self,
        ops: Vec<Op>,
        data: Vec<u8>,
    ) -> Result<(MantleTx, MsgId, Ed25519Signature), Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::PrepareTx {
            ops,
            msg: data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })?
    }

    /// Submit a [`SignedMantleTx`] that is associated with a [`MsgId`]
    pub async fn submit_signed_tx(
        &self,
        tx: SignedMantleTx,
        msg_id: MsgId,
    ) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SubmitSignedTx {
            tx: tx.clone(),
            msg_id,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let result = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        info!(
            "Submitted tx including inscription {:?}",
            result.inscription_id
        );

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(tx).await {
            warn!("Failed to post transaction: {e}");
        }

        Ok(result)
    }

    /// Update the channel's accredited keys.
    ///
    /// The sequencer's signing key must be the channel administrator
    /// (`keys[0]`). This overwrites the entire key list — include the admin
    /// key if it should remain authorized.
    ///
    /// Returns the publish result (with checkpoint) and a future that
    /// resolves when the transaction is finalized:
    ///
    /// ```ignore
    /// let (result, finalized) = handle.set_keys(vec![admin_pk]).await?;
    /// save_checkpoint(&result.checkpoint);
    /// finalized.await?; // wait for finalization
    /// ```
    pub async fn set_keys(
        &self,
        keys: Vec<Ed25519PublicKey>,
    ) -> Result<(PublishResult, impl Future<Output = Result<(), Error>>), Error> {
        // Subscribe BEFORE submitting to avoid missing finalization events.
        let mut event_rx = self.event_tx.subscribe();

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SetKeys {
            keys,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })?;

        let (signed_tx, publish_result) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer dropped reply",
        })??;

        let tx_hash = signed_tx.mantle_tx.hash();

        info!("Submitted set_keys transaction {:?}", tx_hash);

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(signed_tx).await {
            warn!("Failed to post set_keys transaction: {e}");
        }

        let finalized = async move {
            loop {
                match event_rx.recv().await {
                    Ok(Event::TxsFinalized { ref tx_hashes, .. })
                        if tx_hashes.contains(&tx_hash) =>
                    {
                        return Ok(());
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Err(Error::Unavailable {
                            reason: "sequencer stopped",
                        });
                    }
                }
            }
        };

        Ok((publish_result, finalized))
    }
}

/// Zone sequencer.
///
/// The caller drives execution by calling [`next_event`](Self::next_event) in a
/// loop. Publish and admin operations are submitted via the [`SequencerHandle`]
/// which can be used from any task.
pub struct ZoneSequencer<Node> {
    // Config
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node: Node,
    config: SequencerConfig,

    // Actor channel for receiving requests from other tasks
    request_rx: mpsc::Receiver<ActorRequest>,

    // State
    state: Option<TxState>,
    current_tip: Option<HeaderId>,
    lib_slot: Slot,
    last_msg_id: MsgId,

    // Block stream
    blocks_stream: Option<Pin<Box<dyn futures::Stream<Item = ProcessedBlockEvent> + Send>>>,

    // Resubmission
    resubmit_interval: tokio::time::Interval,
    resubmit_active: bool,
    in_flight: FuturesUnordered<BoxFuture<'static, InFlight>>,

    // Buffered event — when both ChannelUpdate and TxsFinalized occur on
    // the same block, one is returned immediately and the other is buffered.
    buffered_event: Option<Event>,

    // Incremental backfill state — processes one batch per next_event() call
    backfill_from: Option<Slot>,
    backfill_to: Option<Slot>,

    // Broadcast channel for events — handles subscribe to receive events
    event_tx: broadcast::Sender<Event>,

    // Readiness signal — set to true when connected and backfill is complete
    ready_tx: tokio::sync::watch::Sender<bool>,
}

impl<Node> ZoneSequencer<Node>
where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    /// Create a new sequencer with default configuration.
    ///
    /// Returns the sequencer (to drive via [`next_event`](Self::next_event))
    /// and a handle (for submitting requests from other tasks).
    ///
    /// For a simpler API that spawns the sequencer automatically, see
    /// [`spawn`](Self::spawn).
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
    /// Returns the sequencer (to drive via [`next_event`](Self::next_event))
    /// and a handle (for submitting requests from other tasks).
    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node: Node,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle<Node>) {
        let (request_tx, request_rx) = mpsc::channel(config.publish_channel_capacity);

        let (state, lib_slot, last_msg_id) = if let Some(cp) = checkpoint {
            info!(
                "Restoring from checkpoint: {} pending txs, lib={:?}, lib_slot={:?}",
                cp.pending_txs.len(),
                cp.lib,
                cp.lib_slot
            );
            let mut tx_state = TxState::new(cp.lib, cp.last_msg_id);
            for (_hash, tx) in cp.pending_txs {
                // Try to extract inscription metadata for lineage tracking
                let mut is_inscription = false;
                for op in &tx.mantle_tx.ops {
                    if let Op::ChannelInscribe(inscribe) = op {
                        tx_state.submit_inscription(
                            tx.clone(),
                            inscribe.parent,
                            inscribe.id(),
                            inscribe.inscription.clone(),
                        );
                        is_inscription = true;
                        break;
                    }
                }
                if !is_inscription {
                    tx_state.submit_other(tx);
                }
            }
            (Some(tx_state), cp.lib_slot, cp.last_msg_id)
        } else {
            info!("Starting fresh (no checkpoint)");
            (None, Slot::genesis(), MsgId::root())
        };

        let resubmit_interval = tokio::time::interval(config.resubmit_interval);
        let (event_tx, _) = broadcast::channel(256);
        let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);

        let handle = SequencerHandle {
            request_tx,
            node: node.clone(),
            event_tx: event_tx.clone(),
            ready_rx,
        };

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
            blocks_stream: None,
            resubmit_interval,
            resubmit_active: false,
            in_flight: FuturesUnordered::new(),
            buffered_event: None,
            backfill_from: None,
            backfill_to: None,
            event_tx,
            ready_tx,
        };

        (sequencer, handle)
    }

    /// Whether the sequencer is connected and ready to accept requests.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        *self.ready_tx.borrow()
    }

    /// Get the current checkpoint for persistence.
    ///
    /// Returns `None` if the sequencer has not yet initialized.
    #[must_use]
    pub fn checkpoint(&self) -> Option<SequencerCheckpoint> {
        self.state
            .as_ref()
            .map(|s| build_checkpoint(s, self.last_msg_id, self.lib_slot))
    }

    /// Spawn the event loop in a background task, consuming the sequencer.
    ///
    /// Use after [`init`](Self::init) or
    /// [`init_with_config`](Self::init_with_config):
    ///
    /// ```ignore
    /// let (sequencer, handle) = ZoneSequencer::init(channel_id, key, url, None, None);
    /// sequencer.spawn();
    /// handle.publish(b"hello".to_vec()).await?;
    /// ```
    pub fn spawn(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                self.next_event().await;
            }
        })
    }

    /// Drive the sequencer and return the next event.
    ///
    /// This processes block events, resubmission, and pending requests.
    /// The caller must call this in a loop to keep the sequencer running.
    pub async fn next_event(&mut self) -> Option<Event> {
        // Return buffered event from previous call if any
        if let Some(event) = self.buffered_event.take() {
            drop(self.event_tx.send(event.clone()));
            return Some(event);
        }

        // Process incremental backfill — one batch per call.
        // Returns Some(Some(event)) or Some(None) while active, None when done.
        if let Some(maybe_event) = self.process_incremental_backfill().await {
            return maybe_event;
        }

        // Ensure we have a blocks stream (connects if needed).
        if !self.ensure_connected().await {
            return None;
        }

        let stream = self.blocks_stream.as_mut()?;

        tokio::select! {
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request).await;
                None
            }
            maybe_event = stream.next() => {
                if let Some(ref block_event) = maybe_event {
                    let result = handle_block_event(
                        block_event,
                        &mut self.state,
                        &mut self.current_tip,
                        &mut self.lib_slot,
                        self.channel_id,
                        &self.node
                    )
                    .await;

                    // Signal readiness after first block event when no
                    // pending startup backfill remains.
                    if !self.is_ready()
                        && self.backfill_from.is_none()
                        && self.backfill_to.is_none()
                    {
                        let _ = self.ready_tx.send(true);
                        drop(self.event_tx.send(Event::Ready));
                    }

                    self.apply_block_result(result)
                } else {
                    warn!("Blocks stream disconnected, will reconnect on next call");
                    self.blocks_stream = None;
                    let _ = self.ready_tx.send(false);
                    None
                }
            }
            Some(inflight_result) = self.in_flight.next(), if !self.in_flight.is_empty() => {
                handle_inflight(inflight_result, &mut self.resubmit_active);
                None
            }
            _ = self.resubmit_interval.tick(), if *self.ready_tx.borrow() && !self.resubmit_active => {
                enqueue_resubmit(
                    self.state.as_ref().unwrap(),
                    self.current_tip.unwrap(),
                    &self.node,
                    &self.in_flight,
                    &mut self.resubmit_active,
                );
                None
            }
        }
    }

    /// Process one batch of incremental backfill if active.
    ///
    /// Returns `Some(event)` while backfill is active (caller should return
    /// the inner value), or `None` when backfill is complete/inactive.
    async fn process_incremental_backfill(&mut self) -> Option<Option<Event>> {
        let (Some(from), Some(to)) = (self.backfill_from, self.backfill_to) else {
            return None;
        };

        let from_u64: u64 = from.into();
        let to_u64: u64 = to.into();

        if from_u64 > to_u64 {
            self.backfill_from = None;
            self.backfill_to = None;
            return None;
        }

        let batch_end = (from_u64 + BACKFILL_BATCH_SIZE).min(to_u64);
        let batch = fetch_and_process_blocks(
            self.state.as_mut().unwrap(),
            from_u64,
            batch_end,
            self.channel_id,
            &self.node,
        )
        .await;

        self.backfill_from = Some(Slot::from(batch_end + 1));

        if let Some(last) = batch.inscriptions.last() {
            self.last_msg_id = last.this_msg;
            if let Some(s) = self.state.as_mut() {
                s.set_finalized_msg(last.this_msg);
            }
        }

        if batch.inscriptions.is_empty() {
            return Some(None);
        }

        let event = Event::FinalizedInscriptions {
            inscriptions: batch.inscriptions,
        };
        drop(self.event_tx.send(event.clone()));
        Some(Some(event))
    }

    /// Ensure the blocks stream is connected. Returns `false` if not yet
    /// ready (caller should return `None`).
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn ensure_connected(&mut self) -> bool {
        if self.blocks_stream.is_some() {
            return true;
        }

        // Initialize state from consensus info if needed
        if self.state.is_none() {
            match self.node.consensus_info().await {
                Ok(info) => {
                    info!(
                        "Sequencer connected: tip={:?}, lib={:?}",
                        info.tip, info.lib
                    );
                    self.state = Some(TxState::new(info.lib, MsgId::root()));
                    self.current_tip = Some(info.tip);
                }
                Err(e) => {
                    warn!("Failed to fetch consensus info: {e}");
                    tokio::time::sleep(self.config.reconnect_delay).await;
                    return false;
                }
            }
        }

        match self.node.block_stream().await {
            Ok(stream) => {
                self.blocks_stream = Some(Box::pin(stream));
            }
            Err(e) => {
                warn!("Failed to connect to blocks stream: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                return false;
            }
        }

        // Check if we need incremental backfill from checkpoint to
        // current network LIB.
        if self.state.is_some() && self.backfill_from.is_none() {
            match self.node.consensus_info().await {
                Ok(info) => {
                    let network_lib_slot = info.lib_slot;
                    let from: u64 = self.lib_slot.into();
                    let to: u64 = network_lib_slot.into();
                    if from < to {
                        debug!("Starting incremental backfill from slot {from} to {to}");
                        self.backfill_from = Some(Slot::from(from + 1));
                        self.backfill_to = Some(network_lib_slot);
                        self.lib_slot = network_lib_slot;
                        return false;
                    }
                }
                Err(e) => {
                    warn!("Failed to fetch consensus info for backfill check: {e}");
                }
            }
        }

        true
    }

    /// Process a `BlockEventResult`: apply channel updates to local state
    /// and emit events. Returns at most one event; a second is buffered.
    fn apply_block_result(&mut self, result: BlockEventResult) -> Option<Event> {
        // Apply channel update to local publish head
        if let Some(ref update) = result.channel_update {
            debug!(
                "ChannelUpdate: invalidated={}, adopted={}, new_tip={:?}",
                update.invalidated.len(),
                update.adopted.len(),
                update.new_channel_tip,
            );
            let has_pending = self
                .state
                .as_ref()
                .is_some_and(TxState::has_pending_inscriptions);

            if !update.invalidated.is_empty() {
                self.last_msg_id = update.new_channel_tip;
                if let Some(s) = self.state.as_mut() {
                    for inv in &update.invalidated {
                        debug!(
                            "Invalidated: payload={:?}",
                            String::from_utf8_lossy(&inv.payload)
                        );
                        s.remove_pending(&inv.tx_hash);
                    }
                }
            } else if !has_pending {
                self.last_msg_id = update.new_channel_tip;
            }
        }

        // Build events
        let channel_event = result.channel_update.map(|u| Event::ChannelUpdate {
            invalidated: u.invalidated,
            adopted: u.adopted,
            new_channel_tip: u.new_channel_tip,
        });
        let finalized_event =
            (!result.finalized_tx_hashes.is_empty()).then_some(Event::TxsFinalized {
                tx_hashes: result.finalized_tx_hashes,
                inscriptions: result.finalized_inscriptions,
            });

        // Emit one event now, buffer the other if both exist
        match (channel_event, finalized_event) {
            (Some(ce), Some(fe)) => {
                self.buffered_event = Some(fe);
                drop(self.event_tx.send(ce.clone()));
                Some(ce)
            }
            (Some(e), None) | (None, Some(e)) => {
                drop(self.event_tx.send(e.clone()));
                Some(e)
            }
            (None, None) => None,
        }
    }

    async fn handle_request(&mut self, request: ActorRequest) {
        if !self.is_ready() {
            match request {
                ActorRequest::PublishMessage { .. } => {
                    warn!("Publish dropped: sequencer not yet ready");
                }
                ActorRequest::SetKeys { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
                ActorRequest::PrepareTx { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
                ActorRequest::SubmitSignedTx { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
            }
            return;
        }

        // Safe to unwrap — is_ready() guarantees state is initialized
        let s = self.state.as_mut().unwrap();

        match request {
            ActorRequest::PublishMessage { data } => {
                // Derive publish parent from state instead of trusting
                // last_msg_id blindly — handles branch switches correctly.
                let parent = if let Some(tip) = self.current_tip {
                    s.publish_parent(tip)
                } else {
                    self.last_msg_id
                };
                debug!(" Publishing with parent={parent:?}");
                let (signed_tx, new_msg_id) =
                    create_inscribe_tx(self.channel_id, &self.signing_key, data.clone(), parent);
                let id = signed_tx.mantle_tx.hash();

                s.submit_inscription(signed_tx.clone(), parent, new_msg_id, data);
                self.last_msg_id = new_msg_id;

                info!("Created inscription {id:?}");

                // Post to network (best effort, resubmit timer retries if needed)
                if let Err(e) = self.node.post_transaction(signed_tx).await {
                    debug!("Failed to post transaction: {e}");
                }

                let checkpoint = build_checkpoint(s, self.last_msg_id, self.lib_slot);
                drop(self.event_tx.send(Event::Published {
                    inscription_id: id,
                    checkpoint,
                }));
            }
            ActorRequest::PrepareTx { ops, msg, reply } => {
                let result = prepare_tx(
                    ops,
                    self.channel_id,
                    &self.signing_key,
                    msg,
                    self.last_msg_id,
                );
                // do not update last_msg_id since tx is not submitted yet
                drop(reply.send(Ok(result)));
            }
            ActorRequest::SubmitSignedTx { tx, msg_id, reply } => {
                let result = submit_signed_tx(s, tx, msg_id, &mut self.last_msg_id, self.lib_slot);
                drop(reply.send(Ok(result)));
            }
            ActorRequest::SetKeys { keys, reply } => {
                let signed_tx = create_set_keys_tx(self.channel_id, &self.signing_key, keys);
                s.submit_other(signed_tx.clone());
                let checkpoint = build_checkpoint(s, self.last_msg_id, self.lib_slot);
                let result = PublishResult {
                    inscription_id: signed_tx.mantle_tx.hash(),
                    checkpoint,
                };
                drop(reply.send(Ok((signed_tx, result))));
            }
        }
    }
}

fn submit_signed_tx(
    state: &mut TxState,
    tx: SignedMantleTx,
    msg_id: MsgId,
    last_msg_id: &mut MsgId,
    lib_slot: Slot,
) -> PublishResult {
    let id = tx.mantle_tx.hash();
    state.submit_other(tx);
    *last_msg_id = msg_id;

    let checkpoint = build_checkpoint(state, *last_msg_id, lib_slot);
    PublishResult {
        inscription_id: id,
        checkpoint,
    }
}

fn build_checkpoint(state: &TxState, last_msg_id: MsgId, lib_slot: Slot) -> SequencerCheckpoint {
    SequencerCheckpoint {
        last_msg_id,
        pending_txs: state.all_pending_txs(),
        lib: state.lib(),
        lib_slot,
    }
}

/// Result of processing a block event.
struct BlockEventResult {
    finalized_tx_hashes: Vec<TxHash>,
    finalized_inscriptions: Vec<InscriptionInfo>,
    channel_update: Option<crate::state::ChannelUpdateInfo>,
}

/// Process a block event. Returns finalized tx hashes and optional channel
/// update.
async fn handle_block_event<Node>(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
    node: &Node,
) -> BlockEventResult
where
    Node: adapter::Node + Sync,
{
    let block_id = event.block.header.id;
    let parent_id = event.block.header.parent_block;
    let tip = event.tip;
    let lib = event.lib;

    // Initialize state on first event
    if state.is_none() {
        *state = Some(TxState::new(lib, MsgId::root()));
    }

    let Some(s) = state.as_mut() else {
        return BlockEventResult {
            finalized_tx_hashes: Vec::new(),
            finalized_inscriptions: Vec::new(),
            channel_update: None,
        };
    };

    let old_tip = *current_tip;

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind)
    let mut lib_finalized = Vec::new();
    if lib != s.lib() {
        let new_lib_slot = event.lib_slot;
        let from: u64 = (*lib_slot).into();
        let to: u64 = new_lib_slot.into();
        if from < to {
            lib_finalized = fetch_and_process_blocks(s, from + 1, to, channel_id, node)
                .await
                .our_tx_hashes;
        }
        *lib_slot = new_lib_slot;
    }

    // 2. Backfill canonical chain if parent is missing
    if !s.has_block(&parent_id) && parent_id != s.lib() {
        backfill_canonical(s, parent_id, channel_id, node).await;
    }

    // Extract tx hashes and inscription info for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_inscriptions(&event.block.transactions, channel_id);

    // Process the actual event block
    s.process_block(block_id, parent_id, lib, our_txs, inscriptions);
    let mut finalized_tx_hashes = Vec::new();
    let mut finalized_inscriptions = Vec::new();

    // Finalize txs found in backfilled LIB blocks — this is ground truth
    // from the node. LIB blocks are truly final (can't be reorged), so
    // txs found there are definitively on the canonical chain. Our safe
    // set may miss them due to gaps or reorgs in the block event stream.
    for tx_hash in &lib_finalized {
        if let Some(tx) = s.remove_pending(tx_hash) {
            finalized_tx_hashes.push(*tx_hash);
            for op in &tx.mantle_tx.ops {
                if let Op::ChannelInscribe(inscribe) = op {
                    debug!(
                        " Backfill-finalized: payload={:?}, tx={tx_hash:?}",
                        String::from_utf8_lossy(&inscribe.inscription)
                    );
                    finalized_inscriptions.push(InscriptionInfo {
                        tx_hash: *tx_hash,
                        parent_msg: inscribe.parent,
                        this_msg: inscribe.id(),
                        payload: inscribe.inscription.clone(),
                    });
                }
            }
        }
    }
    *current_tip = Some(tip);

    // Detect channel changes.
    // On first event (old_tip is None), check for existing inscriptions on
    // the channel — this handles clean start on an existing channel.
    // On subsequent events, detect channel update if tip changed.
    let mut channel_update = match old_tip {
        Some(old) if old != tip => s.detect_channel_update(old, tip),
        None => {
            // First event — check if the channel already has inscriptions.
            // Treat as a reorg from root: the LCM is finalized_msg, and
            // any pending inscriptions chaining from it are orphaned.
            let channel_tip = s.channel_tip_at(tip);
            if channel_tip == MsgId::root() {
                None
            } else {
                let adopted = s.collect_inscriptions_on_branch(tip);
                let invalidated = s.collect_pending_suffix(s.finalized_msg());
                (!adopted.is_empty() || !invalidated.is_empty()).then_some(
                    crate::state::ChannelUpdateInfo {
                        invalidated,
                        adopted,
                        new_channel_tip: channel_tip,
                    },
                )
            }
        }
        _ => None, // tip unchanged
    };

    // On LIB advance, catch stale pending not valid on any branch.
    if !lib_finalized.is_empty() {
        merge_stale_pending(s, tip, &mut channel_update);
    }

    BlockEventResult {
        finalized_tx_hashes,
        finalized_inscriptions,
        channel_update,
    }
}

fn merge_stale_pending(
    s: &TxState,
    tip: HeaderId,
    channel_update: &mut Option<crate::state::ChannelUpdateInfo>,
) {
    let stale = s.collect_stale_pending();
    if stale.is_empty() {
        return;
    }
    if let Some(update) = channel_update {
        let existing: std::collections::HashSet<TxHash> =
            update.invalidated.iter().map(|i| i.tx_hash).collect();
        update
            .invalidated
            .extend(stale.into_iter().filter(|i| !existing.contains(&i.tx_hash)));
    } else {
        *channel_update = Some(crate::state::ChannelUpdateInfo {
            invalidated: stale,
            adopted: Vec::new(),
            new_channel_tip: s.channel_tip_at(tip),
        });
    }
}

fn handle_inflight(event: InFlight, resubmit_active: &mut bool) {
    match event {
        InFlight::ResubmittedBatch { results } => {
            for (id, result) in &results {
                if let Err(e) = result {
                    warn!("Failed to resubmit inscription {id:?}: {e}");
                }
            }
            *resubmit_active = false;
        }
    }
}

/// Result of fetching and processing a slot range.
struct FetchedBatch {
    our_tx_hashes: Vec<TxHash>,
    inscriptions: Vec<InscriptionInfo>,
}

/// Fetch blocks in a slot range, process them into state, and return
/// discovered tx hashes and inscriptions.
async fn fetch_and_process_blocks<Node>(
    state: &mut TxState,
    from_slot: u64,
    to_slot: u64,
    channel_id: ChannelId,
    node: &Node,
) -> FetchedBatch
where
    Node: adapter::Node + Sync,
{
    let mut result = FetchedBatch {
        our_tx_hashes: Vec::new(),
        inscriptions: Vec::new(),
    };

    match node
        .blocks(Slot::from(from_slot), Slot::from(to_slot))
        .await
    {
        Ok(blocks) => {
            for block in blocks {
                let our_txs: Vec<TxHash> = block
                    .transactions
                    .iter()
                    .filter(|tx| matches_channel(tx, channel_id))
                    .map(|tx| tx.mantle_tx.hash())
                    .collect();

                let inscriptions = extract_inscriptions(&block.transactions, channel_id);
                result.our_tx_hashes.extend(our_txs.iter().copied());
                result.inscriptions.extend(inscriptions.clone());

                let current_lib = state.lib();
                state.process_block(
                    block.header.id,
                    block.header.parent_block,
                    current_lib,
                    our_txs,
                    inscriptions,
                );
            }
        }
        Err(e) => {
            warn!("Failed to fetch blocks (slots {from_slot}..{to_slot}): {e}");
        }
    }

    result
}

/// Backfill canonical chain backwards from a missing parent to LIB.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn backfill_canonical<Node>(
    state: &mut TxState,
    missing_parent: HeaderId,
    channel_id: ChannelId,
    node: &Node,
) where
    Node: adapter::Node + Sync,
{
    debug!("Backfilling canonical chain from {:?}", missing_parent);

    let mut blocks_to_process = Vec::new();
    let mut current = missing_parent;
    let lib = state.lib();

    // Walk backwards until we find a known block or reach lib
    while !state.has_block(&current) && current != lib {
        match node.block(current).await {
            Ok(Some(block)) => {
                let parent = block.header().parent_block();
                blocks_to_process.push(block);
                current = parent;
            }
            Ok(None) => {
                warn!("Block {:?} not found during canonical backfill", current);
                break;
            }
            Err(e) => {
                warn!(
                    "Failed to fetch block {:?} during canonical backfill: {e}",
                    current
                );
                break;
            }
        }
    }

    // Process blocks in forward order (oldest first)
    blocks_to_process.reverse();
    for block in blocks_to_process {
        let block_id = block.header().id();
        let parent_id = block.header().parent_block();

        let txs_vec: Vec<_> = block.transactions().cloned().collect();
        let our_txs: Vec<TxHash> = txs_vec
            .iter()
            .filter(|tx| matches_channel(tx, channel_id))
            .map(|tx| tx.mantle_tx.hash())
            .collect();

        let inscriptions = extract_inscriptions(&txs_vec, channel_id);

        // Use current state lib to avoid premature finalization
        state.process_block(block_id, parent_id, lib, our_txs, inscriptions);
    }

    debug!("Canonical backfill complete");
}

fn enqueue_resubmit<Node>(
    state: &TxState,
    tip: HeaderId,
    node: &Node,
    in_flight: &FuturesUnordered<BoxFuture<'static, InFlight>>,
    resubmit_active: &mut bool,
) where
    Node: adapter::Node + Clone + Send + Sync + 'static,
{
    let pending: Vec<(InscriptionId, SignedMantleTx)> = state.pending_txs(tip);

    if pending.is_empty() {
        return;
    }

    debug!("Resubmitting {} pending inscription(s)", pending.len());

    let node = node.clone();
    *resubmit_active = true;

    in_flight.push(Box::pin(async move {
        let mut results = Vec::with_capacity(pending.len());
        for (id, tx) in pending {
            let result = node.post_transaction(tx).await.map_err(|e| e.to_string());
            results.push((id, result));
        }
        InFlight::ResubmittedBatch { results }
    }));
}

/// Extract channel inscription info from a block's transactions, in
/// parent→child chain order. Transactions in a block are not guaranteed
/// to be in chain order, so we topologically sort by inscription lineage.
/// Callers (e.g. `channel_tip_at`) rely on `last()` being the chain tail.
///
/// Panics if the inscriptions for the channel in a single block do not
/// form a single linear chain — that would be a protocol-level invariant
/// violation.
fn extract_inscriptions(txs: &[SignedMantleTx], channel_id: ChannelId) -> Vec<InscriptionInfo> {
    let items: Vec<InscriptionInfo> = txs
        .iter()
        .flat_map(|tx| {
            tx.mantle_tx.ops.iter().filter_map(|op| {
                if let Op::ChannelInscribe(inscribe) = op
                    && inscribe.channel_id == channel_id
                {
                    Some(InscriptionInfo {
                        tx_hash: tx.mantle_tx.hash(),
                        parent_msg: inscribe.parent,
                        this_msg: inscribe.id(),
                        payload: inscribe.inscription.clone(),
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    if items.len() <= 1 {
        return items;
    }

    let this_msgs: std::collections::HashSet<MsgId> = items.iter().map(|i| i.this_msg).collect();
    let by_parent: std::collections::HashMap<MsgId, &InscriptionInfo> =
        items.iter().map(|i| (i.parent_msg, i)).collect();

    // The chain root is the inscription whose parent is not produced
    // within this same block.
    let root = items
        .iter()
        .find(|i| !this_msgs.contains(&i.parent_msg))
        .expect("inscriptions for a channel in a block must form a chain (no root found)");

    let mut sorted = Vec::with_capacity(items.len());
    sorted.push(root.clone());
    let mut current = root.this_msg;
    while let Some(next) = by_parent.get(&current).copied() {
        sorted.push(next.clone());
        current = next.this_msg;
    }
    sorted
}

fn matches_channel(tx: &SignedMantleTx, channel_id: ChannelId) -> bool {
    tx.mantle_tx.ops.iter().any(|op| match op {
        Op::ChannelInscribe(inscribe) => inscribe.channel_id == channel_id,
        Op::ChannelSetKeys(set_keys) => set_keys.channel == channel_id,
        _ => false,
    })
}

fn create_inscribe_tx(
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Vec<u8>,
    parent: MsgId,
) -> (SignedMantleTx, MsgId) {
    let signer = signing_key.public_key();

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer,
    };
    let msg_id = inscribe_op.id();

    // TODO: set realistic gas prices and fund tx
    let inscribe_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscribe_op)],
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

    let tx_hash = inscribe_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: inscribe_tx,
    };

    (signed_tx, msg_id)
}

fn create_set_keys_tx(
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    keys: Vec<Ed25519PublicKey>,
) -> SignedMantleTx {
    let set_keys_op = SetKeysOp {
        channel: channel_id,
        keys,
    };

    // TODO: set realistic gas prices and fund tx
    let set_keys_tx = MantleTx {
        ops: vec![Op::ChannelSetKeys(set_keys_op)],
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

    let tx_hash = set_keys_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

    SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: set_keys_tx,
    }
}

fn prepare_tx(
    mut ops: Vec<Op>,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    inscription: Vec<u8>,
    parent: MsgId,
) -> (MantleTx, MsgId, Ed25519Signature) {
    let inscription_op = InscriptionOp {
        channel_id,
        inscription,
        parent,
        signer: signing_key.public_key(),
    };
    let msg_id = inscription_op.id();
    ops.push(Op::ChannelInscribe(inscription_op));

    // TODO: set realistic gas prices and fund tx
    let tx = MantleTx {
        ops,
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

    let inscription_sig = signing_key.sign_payload(tx.hash().as_signing_bytes().as_ref());

    (tx, msg_id, inscription_sig)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use futures::Stream;
    use lb_common_http_client::{ApiBlock, ApiHeader, BlockInfo, CryptarchiaInfo, State};
    use lb_core::{
        block::Block,
        header::ContentId,
        mantle::{
            Note, Utxo,
            ops::{channel::deposit::DepositOp, transfer::TransferOp},
        },
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_key_management_system_service::keys::ZkKey;
    use num_bigint::BigUint;

    use super::*;
    use crate::ZoneMessage;

    #[tokio::test]
    async fn prepare_submit_deposit_and_inscription() {
        // Init a sequencer
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, mut posted_txs) = MockNode::new();
        let (sequencer, mut handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);
        let _join_handle = sequencer.spawn();
        handle.wait_ready().await;

        // Prepare a deposit op and a transfer op using a depositer's key.
        // The transfer op burns the same amount of tokens as the deposit amount.
        let depositer_key = ZkKey::zero();
        let input_note = Utxo::new(
            TxHash::from(BigUint::ZERO),
            0,
            Note::new(30, depositer_key.to_public_key()),
        );
        let deposit_op = DepositOp {
            channel_id,
            amount: 10,
            metadata: "to Alice".into(),
        };
        let transfer_op = TransferOp {
            inputs: vec![input_note.id()],
            // a change note
            outputs: vec![Note::new(
                input_note.note.value - deposit_op.amount,
                depositer_key.to_public_key(),
            )],
        };

        // Prepare a `MantleTx` with two operations prepared and a inscribe op
        // that presents the zone state transition corresponding to the operations.
        let (tx, msg_id, inscription_sig) = handle
            .prepare_tx(
                vec![
                    Op::ChannelDeposit(deposit_op.clone()),
                    Op::Transfer(transfer_op.clone()),
                ],
                "Mint 10 to Alice".into(),
            )
            .await
            .unwrap();
        assert_eq!(tx.ops.len(), 3);
        assert_eq!(&tx.ops[0], &Op::ChannelDeposit(deposit_op));
        assert_eq!(&tx.ops[1], &Op::Transfer(transfer_op));
        assert!(matches!(&tx.ops[2], &Op::ChannelInscribe(_)));

        // Sign the `MantleTx` with the depositer's key, and put the signature in the
        // 2nd position of proofs since the transfer op is the 2nd op.
        let transfer_sig = depositer_key.sign_payload(tx.hash().as_ref()).unwrap();
        let signed_tx = SignedMantleTx::new(
            tx,
            vec![
                OpProof::NoProof,
                OpProof::ZkSig(transfer_sig),
                OpProof::Ed25519Sig(inscription_sig),
            ],
        )
        .unwrap();

        // Submit the signed tx
        let result = handle
            .submit_signed_tx(signed_tx.clone(), msg_id)
            .await
            .unwrap();
        assert_eq!(result.inscription_id, signed_tx.mantle_tx.hash());
        assert_eq!(result.checkpoint.last_msg_id, msg_id);
        assert_eq!(posted_txs.recv().await.unwrap(), signed_tx);
    }

    #[derive(Clone)]
    struct MockNode {
        posted_transactions_sender: mpsc::Sender<SignedMantleTx>,
    }

    impl MockNode {
        fn new() -> (Self, mpsc::Receiver<SignedMantleTx>) {
            let (tx, rx) = mpsc::channel(10);
            (
                Self {
                    posted_transactions_sender: tx,
                },
                rx,
            )
        }
    }

    #[async_trait]
    impl adapter::Node for MockNode {
        async fn consensus_info(&self) -> Result<CryptarchiaInfo, lb_common_http_client::Error> {
            Ok(CryptarchiaInfo {
                lib: HeaderId::from([0; 32]),
                lib_slot: Slot::genesis(),
                tip: HeaderId::from([0; 32]),
                slot: Slot::genesis(),
                height: 0,
                mode: State::Online,
            })
        }

        async fn block_stream(
            &self,
        ) -> Result<
            impl Stream<Item = ProcessedBlockEvent> + Send + 'static,
            lb_common_http_client::Error,
        > {
            Ok(futures::stream::once(async {
                ProcessedBlockEvent {
                    block: ApiBlock {
                        header: ApiHeader {
                            id: HeaderId::from([1; 32]),
                            parent_block: HeaderId::from([0; 32]),
                            slot: 1.into(),
                            block_root: ContentId::from([0; 32]),
                            proof_of_leadership: Groth16LeaderProof::genesis(),
                        },
                        transactions: Vec::new(),
                    },
                    tip: HeaderId::from([1; 32]),
                    tip_slot: 1.into(),
                    lib: HeaderId::from([0; 32]),
                    lib_slot: Slot::genesis(),
                }
            })
            .chain(futures::stream::pending()))
        }

        async fn lib_stream(
            &self,
        ) -> Result<impl Stream<Item = BlockInfo> + Send, lb_common_http_client::Error> {
            Ok(futures::stream::pending())
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<Block<SignedMantleTx>>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
        ) -> Result<Vec<ApiBlock>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn zone_messages_in_block(
            &self,
            _id: HeaderId,
            _channel_id: ChannelId,
        ) -> Result<impl Stream<Item = ZoneMessage>, lb_common_http_client::Error> {
            Ok(futures::stream::pending())
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<impl Stream<Item = (ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(futures::stream::pending())
        }

        async fn post_transaction(
            &self,
            tx: SignedMantleTx,
        ) -> Result<(), lb_common_http_client::Error> {
            self.posted_transactions_sender.send(tx).await.unwrap();
            Ok(())
        }
    }
}
