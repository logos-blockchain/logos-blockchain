use std::time::Duration;

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{ChainServiceInfo, ProcessedBlockEvent, Slot};
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
    adapter::BoxStream,
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
    /// Consumer pattern:
    /// 1. Apply `orphaned` and `adopted` to state (revert / add).
    /// 2. For each entry in `invalidated`, decide whether to republish.
    ChannelUpdate {
        /// Removed from the canonical branch (revert from state).
        orphaned: Vec<InscriptionInfo>,
        /// Added to the canonical branch (apply to state).
        adopted: Vec<InscriptionInfo>,
        /// Pending tx on this branch.
        pending: Vec<InscriptionInfo>,
        /// Submitted tx that is not valid on this branch anymore.
        invalidated: Vec<InscriptionInfo>,
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
        payload: Vec<u8>,
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

    /// Sign a [`MantleTx`] using the sequencer's key.
    ///
    /// Useful when signing tx built by other sequencers (e.g. withdraw).
    pub async fn sign_tx(&self, tx: &MantleTx) -> Result<Ed25519Signature, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::SignTx {
            tx_hash: tx.hash(),
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

        Ok(result)
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
    blocks_stream: Option<BoxStream<ProcessedBlockEvent>>,

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
    /// Returns immediately. The sequencer emits [`Event::Ready`] once it has
    /// connected and completed backfill.
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
                // Try to extract inscription metadata for lineage tracking.
                // Filter by `channel_id` — a checkpoint can in principle carry
                // txs for other channels if the caller reused it.
                let mut is_inscription = false;
                for op in tx.mantle_tx.ops() {
                    if let Op::ChannelInscribe(inscribe) = op
                        && inscribe.channel_id == channel_id
                    {
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
                self.handle_request(request).await
            }
            maybe_event = stream.next() => {
                self.handle_stream_item(maybe_event).await
            }
            Some(inflight_result) = self.in_flight.next(), if !self.in_flight.is_empty() => {
                handle_inflight(inflight_result, &mut self.resubmit_active);
                None
            }
            _ = self.resubmit_interval.tick(), if self.current_tip.is_some() && !self.resubmit_active => {
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

    /// Handle a single item from the blocks stream. `None` means the stream
    /// disconnected; any other value is processed as a block event.
    async fn handle_stream_item(
        &mut self,
        maybe_event: Option<ProcessedBlockEvent>,
    ) -> Option<Event> {
        let Some(block_event) = maybe_event else {
            warn!("Blocks stream disconnected, will reconnect on next call");
            self.blocks_stream = None;
            let _ = self.ready_tx.send(false);
            return None;
        };

        let result = handle_block_event(
            &block_event,
            &mut self.state,
            &mut self.current_tip,
            &mut self.lib_slot,
            self.channel_id,
            &self.node,
        )
        .await;

        let became_ready = self.maybe_signal_ready();
        let block_event = self.apply_block_result(result);

        if became_ready {
            if let Some(event) = block_event {
                self.buffered_event = Some(event);
            }
            Some(Event::Ready)
        } else {
            block_event
        }
    }

    /// If not yet ready and startup backfill is complete, mark ready and
    /// broadcast `Event::Ready`. Returns true iff readiness transitioned.
    fn maybe_signal_ready(&self) -> bool {
        if self.is_ready() {
            return false;
        }
        if self.backfill_from.is_none() && self.backfill_to.is_none() {
            debug!("Sequencer ready (backfill complete, first block processed)");
            let _ = self.ready_tx.send(true);
            drop(self.event_tx.send(Event::Ready));
            true
        } else {
            debug!(
                "Not yet ready: backfill_from={:?}, backfill_to={:?}",
                self.backfill_from, self.backfill_to
            );
            false
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
            // Backfill exhausted — range advanced past `to` in a previous batch.
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
    async fn ensure_connected(&mut self) -> bool {
        if self.blocks_stream.is_some() {
            return true;
        }
        debug!("ensure_connected: connecting...");

        if !self.init_state_if_needed().await {
            return false;
        }
        if !self.open_block_stream().await {
            return false;
        }
        if !self.setup_backfill_range().await {
            return false;
        }
        true
    }

    /// Initialize `self.state` from consensus info on cold start. `current_tip`
    /// stays None so the first live block event emits everything from LIB up to
    /// the new tip as `adopted`. On reconnect this is a no-op.
    async fn init_state_if_needed(&mut self) -> bool {
        if self.state.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                info!(
                    "Sequencer connected: tip={:?}, lib={:?}",
                    cryptarchia_info.tip, cryptarchia_info.lib
                );
                self.state = Some(TxState::new(cryptarchia_info.lib, MsgId::root()));
                true
            }
            Err(e) => {
                warn!("Failed to fetch consensus info: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    async fn open_block_stream(&mut self) -> bool {
        debug!("ensure_connected: opening blocks stream...");
        match self.node.block_stream().await {
            Ok(stream) => {
                debug!("ensure_connected: blocks stream connected");
                self.blocks_stream = Some(stream);
                true
            }
            Err(e) => {
                warn!("Failed to connect to blocks stream: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    /// Check whether an incremental backfill range is needed (checkpoint lib
    /// behind current network lib). Returns `false` if a backfill was set up
    /// (caller defers readiness until backfill completes).
    async fn setup_backfill_range(&mut self) -> bool {
        if self.state.is_none() || self.backfill_from.is_some() {
            return true;
        }
        match self.node.consensus_info().await {
            Ok(ChainServiceInfo {
                cryptarchia_info, ..
            }) => {
                let network_lib_slot = cryptarchia_info.lib_slot;
                let from: u64 = self.lib_slot.into();
                let to: u64 = network_lib_slot.into();
                if from < to {
                    debug!("Starting incremental backfill from slot {from} to {to}");
                    self.backfill_from = Some(Slot::from(from + 1));
                    self.backfill_to = Some(network_lib_slot);
                    self.lib_slot = network_lib_slot;
                    return false;
                }
                true
            }
            Err(e) => {
                warn!("Failed to fetch consensus info for backfill check: {e}");
                true
            }
        }
    }

    /// Process a `BlockEventResult`: apply channel updates to local state
    /// and emit events. Returns at most one event; a second is buffered.
    fn apply_block_result(&mut self, result: BlockEventResult) -> Option<Event> {
        if let Some(update) = result.channel_update.as_ref() {
            Self::log_channel_update(update);
            let has_pending = self
                .state
                .as_ref()
                .is_some_and(TxState::has_pending_inscriptions);
            if !update.orphaned.is_empty() || !has_pending {
                self.last_msg_id = update.new_channel_tip;
            }
        }

        let channel_event = result.channel_update.map(|u| self.build_channel_event(u));

        let finalized_event = (!result.finalized_tx_hashes.is_empty()
            || !result.finalized_inscriptions.is_empty())
        .then_some(Event::TxsFinalized {
            tx_hashes: result.finalized_tx_hashes,
            inscriptions: result.finalized_inscriptions,
        });

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

    fn log_channel_update(update: &crate::state::ChannelUpdateInfo) {
        debug!(
            "ChannelUpdate: orphaned={}, adopted={}, new_tip={:?}",
            update.orphaned.len(),
            update.adopted.len(),
            update.new_channel_tip,
        );
        for inv in &update.orphaned {
            debug!(
                "  orphaned: payload={:?}, tx={:?}, msg_id={:?}",
                String::from_utf8_lossy(&inv.payload),
                inv.tx_hash,
                inv.this_msg,
            );
        }
        for inv in &update.adopted {
            debug!(
                "  adopted: payload={:?}, tx={:?}, msg_id={:?}",
                String::from_utf8_lossy(&inv.payload),
                inv.tx_hash,
                inv.this_msg,
            );
        }
    }

    /// Build the `ChannelUpdate` event. `invalidated` = orphaned blocks ∪
    /// pending shed because lineage no longer reaches the new channel tip.
    /// Shed runs here (only when there's a canonical change) — pre-event
    /// state is preserved so shed can correctly identify what just went
    /// off-branch.
    fn build_channel_event(&mut self, u: crate::state::ChannelUpdateInfo) -> Event {
        let shed = match (self.state.as_mut(), self.current_tip) {
            (Some(s), Some(tip)) => s.shed_off_branch_pending(tip),
            _ => Vec::new(),
        };
        let pending = match (self.state.as_ref(), self.current_tip) {
            (Some(s), Some(tip)) => s.pending_on_branch(tip),
            _ => Vec::new(),
        };

        let orphaned_hashes: std::collections::HashSet<TxHash> =
            u.orphaned.iter().map(|i| i.tx_hash).collect();
        let mut invalidated = u.orphaned.clone();
        invalidated.extend(
            shed.into_iter()
                .filter(|i| !orphaned_hashes.contains(&i.tx_hash)),
        );

        for inv in &pending {
            debug!(
                "  pending: payload={:?}, tx={:?}, msg_id={:?}, parent={:?}",
                String::from_utf8_lossy(&inv.payload),
                inv.tx_hash,
                inv.this_msg,
                inv.parent_msg,
            );
        }
        for inv in &invalidated {
            debug!(
                "  invalidated: payload={:?}, tx={:?}, msg_id={:?}",
                String::from_utf8_lossy(&inv.payload),
                inv.tx_hash,
                inv.this_msg,
            );
        }

        Event::ChannelUpdate {
            orphaned: u.orphaned,
            adopted: u.adopted,
            pending,
            invalidated,
            new_channel_tip: u.new_channel_tip,
        }
    }

    async fn handle_request(&mut self, request: ActorRequest) -> Option<Event> {
        if !self.is_ready() {
            reject_not_ready(request);
            return None;
        }

        match request {
            ActorRequest::PublishMessage { data } => Some(self.handle_publish(data).await),
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
                None
            }
            ActorRequest::SignTx { tx_hash, reply } => {
                let signature = sign_tx(tx_hash, &self.signing_key);
                drop(reply.send(Ok(signature)));
                None
            }
            ActorRequest::SubmitSignedTx { tx, msg_id, reply } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let result = submit_signed_tx(s, tx, msg_id, &mut self.last_msg_id, self.lib_slot);
                drop(reply.send(Ok(result)));
                None
            }
            ActorRequest::SetKeys { keys, reply } => {
                // Safe to unwrap — is_ready() guarantees state is initialized
                let s = self.state.as_mut().unwrap();
                let signed_tx = create_set_keys_tx(self.channel_id, &self.signing_key, keys);
                s.submit_other(signed_tx.clone());
                let checkpoint = build_checkpoint(s, self.last_msg_id, self.lib_slot);
                let result = PublishResult {
                    inscription_id: signed_tx.mantle_tx.hash(),
                    checkpoint,
                };
                drop(reply.send(Ok((signed_tx, result))));
                None
            }
        }
    }

    async fn handle_publish(&mut self, data: Vec<u8>) -> Event {
        // Safe to unwrap — handle_request checks is_ready() first
        let s = self.state.as_mut().unwrap();

        // Derive publish parent from state instead of trusting
        // last_msg_id blindly — handles branch switches correctly.
        let parent = if let Some(tip) = self.current_tip {
            s.publish_parent(tip)
        } else {
            self.last_msg_id
        };
        let (signed_tx, new_msg_id) =
            create_inscribe_tx(self.channel_id, &self.signing_key, data.clone(), parent);
        let id = signed_tx.mantle_tx.hash();

        debug!(
            "Publishing: payload={:?}, parent={parent:?}, msg_id={new_msg_id:?}, tx={id:?}",
            String::from_utf8_lossy(&data),
        );

        s.submit_inscription(signed_tx.clone(), parent, new_msg_id, data.clone());
        self.last_msg_id = new_msg_id;

        // Post to network (best effort, resubmit timer retries if needed)
        if let Err(e) = self.node.post_transaction(signed_tx).await {
            debug!("Failed to post transaction: {e}");
        }

        let checkpoint = build_checkpoint(s, self.last_msg_id, self.lib_slot);
        let event = Event::Published {
            inscription_id: id,
            payload: data,
            checkpoint,
        };
        drop(self.event_tx.send(event.clone()));
        event
    }
}

fn reject_not_ready(request: ActorRequest) {
    let err = || Error::Unavailable {
        reason: "sequencer not yet ready",
    };
    match request {
        ActorRequest::PublishMessage { .. } => {
            warn!("Publish dropped: sequencer not yet ready");
        }
        ActorRequest::SetKeys { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::PrepareTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SignTx { reply, .. } => drop(reply.send(Err(err()))),
        ActorRequest::SubmitSignedTx { reply, .. } => drop(reply.send(Err(err()))),
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
    let mut lib_inscriptions = Vec::new();
    if lib != s.lib() {
        let new_lib_slot = event.lib_slot;
        let from: u64 = (*lib_slot).into();
        let to: u64 = new_lib_slot.into();
        if from < to {
            let batch = fetch_and_process_blocks(s, from + 1, to, channel_id, node).await;
            lib_finalized = batch.our_tx_hashes;
            lib_inscriptions = batch.inscriptions;
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

    // Remove our pending txs that were finalized in backfilled LIB blocks.
    let mut finalized_tx_hashes = Vec::new();
    for tx_hash in &lib_finalized {
        if s.remove_pending(tx_hash).is_some() {
            finalized_tx_hashes.push(*tx_hash);
        }
    }

    // All channel inscriptions from backfilled LIB blocks — includes both
    // our own and other sequencers' inscriptions. Consumers need the full
    // picture to update their local state correctly.
    let finalized_inscriptions = lib_inscriptions;
    for info in &finalized_inscriptions {
        tracing::trace!(
            " Backfill-finalized: payload={:?}, tx={:?}",
            String::from_utf8_lossy(&info.payload),
            info.tx_hash
        );
    }
    *current_tip = Some(tip);

    // Detect channel changes.
    // On first event (old_tip is None), check for existing inscriptions on
    // the channel — this handles clean start on an existing channel.
    // On subsequent events, detect channel update if tip changed.
    let channel_update = match old_tip {
        Some(old) if old != tip => s.detect_channel_update(old, tip),
        None => {
            // First event — no old canonical exists yet, so nothing can be
            // orphaned. Report any inscriptions on the initial tip as adopted.
            let channel_tip = s.channel_tip_at(tip);
            if channel_tip == MsgId::root() {
                None
            } else {
                let adopted = s.collect_inscriptions_on_branch(tip);
                (!adopted.is_empty()).then_some(crate::state::ChannelUpdateInfo {
                    orphaned: Vec::new(),
                    adopted,
                    new_channel_tip: channel_tip,
                })
            }
        }
        _ => None, // tip unchanged
    };

    BlockEventResult {
        finalized_tx_hashes,
        finalized_inscriptions,
        channel_update,
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
        .immutable_blocks(Slot::from(from_slot), Slot::from(to_slot))
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
async fn backfill_canonical<Node>(
    state: &mut TxState,
    missing_parent: HeaderId,
    channel_id: ChannelId,
    node: &Node,
) where
    Node: adapter::Node + Sync,
{
    debug!("Backfilling canonical chain from {:?}", missing_parent);
    let blocks = walk_back_to_known(state, missing_parent, node).await;
    let lib = state.lib();
    for block in &blocks {
        apply_backfilled_block(state, block, channel_id, lib);
    }
    debug!("Canonical backfill complete");
}

/// Walk backwards from `from` until a block the state already knows about (or
/// LIB) is reached. Returns blocks in forward order (oldest first).
async fn walk_back_to_known<Node>(
    state: &TxState,
    from: HeaderId,
    node: &Node,
) -> Vec<lb_common_http_client::ApiBlock>
where
    Node: adapter::Node + Sync,
{
    let mut blocks = Vec::new();
    let mut current = from;
    let lib = state.lib();

    while !state.has_block(&current) && current != lib {
        match node.block(current).await {
            Ok(Some(block)) => {
                let parent = block.header.parent_block;
                blocks.push(block);
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

    blocks.reverse();
    blocks
}

fn apply_backfilled_block(
    state: &mut TxState,
    block: &lb_common_http_client::ApiBlock,
    channel_id: ChannelId,
    lib: HeaderId,
) {
    let block_id = block.header.id;
    let parent_id = block.header.parent_block;

    let our_txs: Vec<TxHash> = block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    let inscriptions = extract_inscriptions(&block.transactions, channel_id);

    // Use current state lib to avoid premature finalization
    state.process_block(block_id, parent_id, lib, our_txs, inscriptions);
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

    for (id, tx) in &pending {
        let payloads: Vec<String> = tx
            .mantle_tx
            .ops()
            .iter()
            .filter_map(|op| {
                if let Op::ChannelInscribe(ins) = op {
                    Some(String::from_utf8_lossy(&ins.inscription).to_string())
                } else {
                    None
                }
            })
            .collect();
        debug!("  resubmit: tx={id:?}, payloads={payloads:?}");
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
            tx.mantle_tx.ops().iter().filter_map(|op| {
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
    tx.mantle_tx.ops().iter().any(|op| match op {
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
    let inscribe_tx = MantleTx(vec![Op::ChannelInscribe(inscribe_op)]);

    let tx_hash = inscribe_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);

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

    // TODO: fund tx
    let set_keys_tx = MantleTx(vec![Op::ChannelSetKeys(set_keys_op)]);

    let tx_hash = set_keys_tx.hash();
    let signature = sign_tx(tx_hash, signing_key);

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

    // TODO: fund tx
    let tx = MantleTx(ops);

    let inscription_sig = sign_tx(tx.hash(), signing_key);

    (tx, msg_id, inscription_sig)
}

fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref())
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use async_trait::async_trait;
    use lb_common_http_client::{
        ApiBlock, ApiHeader, BlockInfo, ChainServiceMode, CryptarchiaInfo, State,
    };
    use lb_core::{
        header::ContentId,
        mantle::{Note, Utxo, ledger::Inputs, ops::channel::deposit::DepositOp},
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_key_management_system_service::keys::ZkKey;
    use num_bigint::BigUint;
    use rand::{RngCore as _, thread_rng};

    use super::*;
    use crate::ZoneMessage;

    #[must_use]
    pub fn utxo_with_sk() -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(BigUint::from(0u64));
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(10, zk_sk.to_public_key()),
        };

        (zk_sk, utxo)
    }

    #[tokio::test]
    async fn prepare_submit_deposit_and_inscription() {
        // Init a sequencer
        let channel_id = ChannelId::from([0; 32]);
        let sequencer_key = Ed25519Key::from_bytes(&[0; 32]);
        let (node, mut posted_txs) = MockNode::new();
        let (mut sequencer, handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);

        // Drive sequencer until ready
        loop {
            if matches!(sequencer.next_event().await, Some(Event::Ready)) {
                break;
            }
        }

        // Prepare a deposit op
        let (sk, utxo) = utxo_with_sk();
        let deposit_op = DepositOp {
            channel_id,
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: "to Alice".into(),
        };

        // Prepare a `MantleTx` — drive sequencer concurrently to process the request
        let prepare_fut = handle.prepare_tx(
            vec![Op::ChannelDeposit(deposit_op.clone())],
            "Mint 10 to Alice".into(),
        );
        tokio::pin!(prepare_fut);
        let (tx, msg_id, inscription_sig) = loop {
            tokio::select! {
                result = &mut prepare_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
        assert_eq!(tx.ops().len(), 2);
        assert_eq!(&tx.ops()[0], &Op::ChannelDeposit(deposit_op));
        assert!(matches!(&tx.ops()[1], &Op::ChannelInscribe(_)));

        // Sign the `MantleTx`
        let signed_tx = SignedMantleTx::new(
            tx.clone(),
            vec![
                OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(&sk), &tx.clone().hash().to_fr())
                        .unwrap(),
                ),
                OpProof::Ed25519Sig(inscription_sig),
            ],
        )
        .unwrap();

        // Submit the signed tx — drive sequencer concurrently to process
        let submit_fut = handle.submit_signed_tx(signed_tx.clone(), msg_id);
        tokio::pin!(submit_fut);
        let result = loop {
            tokio::select! {
                result = &mut submit_fut => break result.unwrap(),
                _ = sequencer.next_event() => {}
            }
        };
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
        async fn consensus_info(&self) -> Result<ChainServiceInfo, lb_common_http_client::Error> {
            Ok(ChainServiceInfo {
                cryptarchia_info: CryptarchiaInfo {
                    lib: HeaderId::from([0; 32]),
                    lib_slot: Slot::genesis(),
                    tip: HeaderId::from([0; 32]),
                    slot: Slot::genesis(),
                    height: 0,
                },
                mode: ChainServiceMode::Started(State::Online),
            })
        }

        async fn block_stream(
            &self,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            Ok(Box::pin(
                futures::stream::once(async {
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
                .chain(futures::stream::pending()),
            ))
        }

        async fn blocks_range_stream(
            &self,
            _blocks_limit: Option<NonZero<usize>>,
            _slot_from: Option<u64>,
            _slot_to: Option<u64>,
            _descending: Option<bool>,
            _server_batch_size: Option<NonZero<usize>>,
            _immutable_only: Option<bool>,
        ) -> Result<BoxStream<ProcessedBlockEvent>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn lib_stream(&self) -> Result<BoxStream<BlockInfo>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn block(
            &self,
            _id: HeaderId,
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
            unimplemented!()
        }

        async fn immutable_blocks(
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
        ) -> Result<BoxStream<ZoneMessage>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn zone_messages_in_blocks(
            &self,
            _slot_from: Slot,
            _slot_to: Slot,
            _channel_id: ChannelId,
        ) -> Result<BoxStream<(ZoneMessage, Slot)>, lb_common_http_client::Error> {
            Ok(Box::pin(futures::stream::pending()))
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
