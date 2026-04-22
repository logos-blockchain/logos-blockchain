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

use crate::{adapter, state::TxState};

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;

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
    TxsFinalized { tx_hashes: Vec<TxHash> },
}

enum ActorRequest {
    /// Create/sign/submit a transaction with an inscription
    PublishMessage {
        data: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
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

    /// Create/sign/submit a transaction with an inscription for the given
    /// message to the zone's channel.
    ///
    /// Returns the inscription ID and a checkpoint for persistence.
    pub async fn publish_message(&self, data: Vec<u8>) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::PublishMessage {
            data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "sequencer channel closed",
            })?;

        let (signed_tx, result) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer dropped reply",
        })??;

        info!("Created inscription {:?}", result.inscription_id);

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self.node.post_transaction(signed_tx).await {
            warn!("Failed to post transaction: {e}");
        }

        Ok(result)
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
                    Ok(Event::TxsFinalized { ref tx_hashes }) if tx_hashes.contains(&tx_hash) => {
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

    // Buffered events to deliver

    // Broadcast channel for events — handles subscribe to receive events
    event_tx: broadcast::Sender<Event>,

    // Readiness signal — set to true after first block event processed
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
            let mut tx_state = TxState::new(cp.lib);
            for (_hash, tx) in cp.pending_txs {
                tx_state.submit(tx);
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
            event_tx,
            ready_tx,
        };

        (sequencer, handle)
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

    /// Whether the sequencer is connected and ready to accept requests.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        *self.ready_tx.borrow()
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
        if self.blocks_stream.is_none() && !self.ensure_connected().await {
            return None;
        }

        let stream = self.blocks_stream.as_mut()?;

        tokio::select! {
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request);
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

                    // Update channel tip from backfill/block inscriptions.
                    // Only when no pending inscriptions remain — if there are
                    // pending txs, the checkpoint's last_msg_id may be ahead
                    // of backfill (inscriptions above LIB, not yet finalized).
                    if let Some(tip) = result.channel_tip {
                        let has_pending = self
                            .state
                            .as_ref()
                            .is_some_and(|s| s.unfinalized_count() > 0);
                        if !has_pending {
                            self.last_msg_id = tip;
                        }
                    }

                    // Signal readiness after first block event processed
                    if !self.is_ready() {
                        let _ = self.ready_tx.send(true);
                    }

                    if result.newly_finalized.is_empty() {
                        None
                    } else {
                        let event = Event::TxsFinalized { tx_hashes: result.newly_finalized };
                        drop(self.event_tx.send(event.clone()));
                        Some(event)
                    }
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

    /// Ensure the blocks stream is connected. Returns `false` if not yet
    /// ready (caller should return `None`).
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn ensure_connected(&mut self) -> bool {
        if self.state.is_none() {
            match self.node.consensus_info().await {
                Ok(info) => {
                    info!(
                        "Sequencer connected: tip={:?}, lib={:?}",
                        info.tip, info.lib
                    );
                    self.state = Some(TxState::new(info.lib));
                    self.current_tip = Some(info.tip);
                    // Do NOT update lib_slot here for fresh starts.
                    // Keep it at genesis so the backfill check in
                    // handle_block_event detects the gap and catches
                    // up on existing channel inscriptions.
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
                true
            }
            Err(e) => {
                warn!("Failed to connect to blocks stream: {e}");
                tokio::time::sleep(self.config.reconnect_delay).await;
                false
            }
        }
    }

    fn handle_request(&mut self, request: ActorRequest) {
        if !self.is_ready() {
            match request {
                ActorRequest::PublishMessage { reply, .. }
                | ActorRequest::SetKeys { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
                ActorRequest::PrepareTx { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
                ActorRequest::SignTx { reply, .. } => {
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
            ActorRequest::PublishMessage { data, reply } => {
                let (signed_tx, new_msg_id) =
                    create_inscribe_tx(self.channel_id, &self.signing_key, data, self.last_msg_id);
                let result = submit_signed_tx(
                    s,
                    signed_tx.clone(),
                    new_msg_id,
                    &mut self.last_msg_id,
                    self.lib_slot,
                );
                drop(reply.send(Ok((signed_tx, result))));
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
            ActorRequest::SignTx { tx_hash, reply } => {
                let signature = sign_tx(tx_hash, &self.signing_key);
                drop(reply.send(Ok(signature)));
            }
            ActorRequest::SubmitSignedTx { tx, msg_id, reply } => {
                let result = submit_signed_tx(s, tx, msg_id, &mut self.last_msg_id, self.lib_slot);
                drop(reply.send(Ok(result)));
            }
            ActorRequest::SetKeys { keys, reply } => {
                let signed_tx = create_set_keys_tx(self.channel_id, &self.signing_key, keys);
                s.submit(signed_tx.clone());
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
    state.submit(tx);
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
        pending_txs: state
            .all_pending_txs()
            .map(|(h, tx)| (*h, tx.clone()))
            .collect(),
        lib: state.lib(),
        lib_slot,
    }
}

/// Result of processing a block event.
struct BlockEventResult {
    newly_finalized: Vec<TxHash>,
    /// Latest channel inscription `MsgId` seen during backfill/processing.
    channel_tip: Option<MsgId>,
}

/// Process a block event.
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
        *state = Some(TxState::new(lib));
    }

    let Some(s) = state.as_mut() else {
        return BlockEventResult {
            newly_finalized: Vec::new(),
            channel_tip: None,
        };
    };

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind)
    let mut channel_tip = None;
    if lib != s.lib() {
        let new_lib_slot = event.lib_slot;
        if *lib_slot < new_lib_slot
            && let Some(tip) = backfill_to_lib(s, *lib_slot, new_lib_slot, channel_id, node).await
        {
            channel_tip = Some(tip);
        }
        *lib_slot = new_lib_slot;
    }

    // 2. Backfill canonical chain if parent is missing
    if !s.has_block(&parent_id) && parent_id != s.lib() {
        backfill_canonical(s, parent_id, channel_id, node).await;
    }

    // Extract tx hashes and latest inscription for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    if let Some(tip) = find_channel_tip(&event.block.transactions, channel_id) {
        channel_tip = Some(tip);
    }

    let newly_finalized = s.process_block(block_id, parent_id, lib, our_txs);
    *current_tip = Some(tip);

    BlockEventResult {
        newly_finalized,
        channel_tip,
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

/// Backfill finalized blocks from current `lib_slot` to new `lib_slot`.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
/// Returns the latest channel inscription `MsgId` found during backfill.
#[expect(
    clippy::cognitive_complexity,
    reason = "TODO: address this in a dedicated refactor"
)]
async fn backfill_to_lib<Node>(
    state: &mut TxState,
    from_slot: Slot,
    to_slot: Slot,
    channel_id: ChannelId,
    node: &Node,
) -> Option<MsgId>
where
    Node: adapter::Node + Sync,
{
    if from_slot >= to_slot {
        return None;
    }

    debug!(
        "Backfilling finalized blocks from {:?} to {:?}",
        from_slot + 1,
        to_slot
    );

    let mut latest_msg_id = None;

    match node.blocks(from_slot + 1, to_slot).await {
        Ok(blocks) => {
            for block in blocks {
                let block_id = block.header.id;
                let parent_id = block.header.parent_block;

                let our_txs: Vec<TxHash> = block
                    .transactions
                    .iter()
                    .filter(|tx| matches_channel(tx, channel_id))
                    .map(|tx| tx.mantle_tx.hash())
                    .collect();

                if let Some(tip) = find_channel_tip(&block.transactions, channel_id) {
                    latest_msg_id = Some(tip);
                }

                let current_lib = state.lib();
                state.process_block(block_id, parent_id, current_lib, our_txs);
            }
            debug!(
                "Backfilled {} finalized blocks",
                to_slot.into_inner() - from_slot.into_inner()
            );
        }
        Err(e) => {
            warn!("Failed to backfill finalized blocks: {e}");
        }
    }

    latest_msg_id
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
                let parent = block.header.parent_block;
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
        let block_id = block.header.id;
        let parent_id = block.header.parent_block;

        let our_txs: Vec<TxHash> = block
            .transactions
            .iter()
            .filter(|tx| matches_channel(tx, channel_id))
            .map(|tx| tx.mantle_tx.hash())
            .collect();

        // Use current state lib to avoid premature finalization
        state.process_block(block_id, parent_id, lib, our_txs);
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
    let pending: Vec<(InscriptionId, SignedMantleTx)> = state
        .pending_txs(tip)
        .map(|(hash, tx)| (*hash, tx.clone()))
        .collect();

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

/// Find the latest channel inscription in a block's transactions.
/// Transactions are guaranteed to be in channel order by the node.
fn find_channel_tip(txs: &[SignedMantleTx], channel_id: ChannelId) -> Option<MsgId> {
    txs.iter()
        .rev()
        .flat_map(|tx| tx.mantle_tx.ops.iter().rev())
        .find_map(|op| {
            if let Op::ChannelInscribe(inscribe) = op
                && inscribe.channel_id == channel_id
            {
                Some(inscribe.id())
            } else {
                None
            }
        })
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

    // TODO: set realistic gas prices and fund tx
    let set_keys_tx = MantleTx {
        ops: vec![Op::ChannelSetKeys(set_keys_op)],
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

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

    // TODO: set realistic gas prices and fund tx
    let tx = MantleTx {
        ops,
        storage_gas_price: 0.into(),
        execution_gas_price: 0.into(),
    };

    let inscription_sig = sign_tx(tx.hash(), signing_key);

    (tx, msg_id, inscription_sig)
}

fn sign_tx(tx_hash: TxHash, signing_key: &Ed25519Key) -> Ed25519Signature {
    signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use futures::Stream;
    use lb_common_http_client::{ApiBlock, ApiHeader, BlockInfo, CryptarchiaInfo, State};
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
        let (sequencer, mut handle) = ZoneSequencer::init(channel_id, sequencer_key, node, None);
        let _join_handle = sequencer.spawn();
        handle.wait_ready().await;

        // Prepare a deposit op
        let (sk, utxo) = utxo_with_sk();
        let deposit_op = DepositOp {
            channel_id,
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: "to Alice".into(),
        };

        // Prepare a `MantleTx` with the deposit and the inscribe op
        // that presents the zone state transition corresponding to the operations.
        let (tx, msg_id, inscription_sig) = handle
            .prepare_tx(
                vec![Op::ChannelDeposit(deposit_op.clone())],
                "Mint 10 to Alice".into(),
            )
            .await
            .unwrap();
        assert_eq!(tx.ops.len(), 2);
        assert_eq!(&tx.ops[0], &Op::ChannelDeposit(deposit_op));
        assert!(matches!(&tx.ops[1], &Op::ChannelInscribe(_)));

        // Sign the `MantleTx`
        let signed_tx = SignedMantleTx::new(
            tx.clone(),
            vec![
                OpProof::ZkSig(
                    ZkKey::multi_sign(std::slice::from_ref(&sk), tx.clone().hash().as_ref())
                        .unwrap(),
                ),
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
        ) -> Result<Option<ApiBlock>, lb_common_http_client::Error> {
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
