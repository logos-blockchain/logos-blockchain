use std::{pin::Pin, time::Duration};

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient, ProcessedBlockEvent, Slot};
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
use lb_key_management_system_service::keys::Ed25519Key;
use reqwest::Url;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use crate::state::{InscriptionInfo, TxState};

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
    TxsFinalized { tx_hashes: Vec<TxHash> },
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
}

enum ActorRequest {
    Publish {
        data: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
    SetKeys {
        keys: Vec<Ed25519PublicKey>,
        reply: tokio::sync::oneshot::Sender<Result<SignedMantleTx, Error>>,
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
pub struct SequencerHandle {
    request_tx: mpsc::Sender<ActorRequest>,
    node_url: Url,
    http_client: CommonHttpClient,
    event_tx: broadcast::Sender<Event>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
}

impl SequencerHandle {
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
    /// Returns the inscription ID and a checkpoint for persistence.
    pub async fn publish(&self, data: Vec<u8>) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let request = ActorRequest::Publish {
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
        if let Err(e) = self
            .http_client
            .post_transaction(self.node_url.clone(), signed_tx)
            .await
        {
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
    /// Returns a future that resolves when the transaction is finalized.
    /// The first `.await` submits the transaction, the second `.await`
    /// waits for finalization:
    ///
    /// ```ignore
    /// let finalized = handle.set_keys(vec![admin_pk]).await?;
    /// finalized.await?; // wait for finalization
    /// ```
    pub async fn set_keys(
        &self,
        keys: Vec<Ed25519PublicKey>,
    ) -> Result<impl Future<Output = Result<(), Error>>, Error> {
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

        let signed_tx = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "sequencer dropped reply",
        })??;

        let tx_hash = signed_tx.mantle_tx.hash();

        // Subscribe to events BEFORE posting to avoid a race where the tx
        // finalizes between posting and subscribing.
        let mut event_rx = self.event_tx.subscribe();

        info!("Submitted set_keys transaction {:?}", tx_hash);

        // Post to network (best effort, will be resubmitted if needed)
        if let Err(e) = self
            .http_client
            .post_transaction(self.node_url.clone(), signed_tx)
            .await
        {
            warn!("Failed to post set_keys transaction: {e}");
        }

        Ok(async move {
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
        })
    }
}

/// Zone sequencer.
///
/// The caller drives execution by calling [`next_event`](Self::next_event) in a
/// loop. Publish and admin operations are submitted via the [`SequencerHandle`]
/// which can be used from any task.
pub struct ZoneSequencer {
    // Config
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node_url: Url,
    http_client: CommonHttpClient,
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

impl ZoneSequencer {
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
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle) {
        Self::init_with_config(
            channel_id,
            signing_key,
            node_url,
            auth,
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
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> (Self, SequencerHandle) {
        let http_client = CommonHttpClient::new(auth);
        let (request_tx, request_rx) = mpsc::channel(config.publish_channel_capacity);

        let (state, lib_slot, last_msg_id) = if let Some(cp) = checkpoint {
            info!(
                "Restoring from checkpoint: {} pending txs, lib={:?}, lib_slot={:?}",
                cp.pending_txs.len(),
                cp.lib,
                cp.lib_slot
            );
            let mut tx_state = TxState::new(cp.lib, cp.last_msg_id);
            for (hash, tx) in cp.pending_txs {
                // Try to extract inscription metadata for lineage tracking
                let mut is_inscription = false;
                for op in &tx.mantle_tx.ops {
                    if let Op::ChannelInscribe(inscribe) = op {
                        tx_state.submit_inscription(
                            hash,
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
                    tx_state.submit_other(hash, tx);
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
            node_url: node_url.clone(),
            http_client: http_client.clone(),
            event_tx: event_tx.clone(),
            ready_rx,
        };

        let sequencer = Self {
            channel_id,
            signing_key,
            node_url,
            http_client,
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

    /// Spawn the sequencer in a background task and return a handle.
    ///
    /// This is a convenience wrapper around
    /// [`init_with_config`](Self::init_with_config) for users who don't
    /// need to drive the event loop manually.
    ///
    /// ```ignore
    /// let handle = ZoneSequencer::spawn(channel_id, key, url, None, None, None);
    /// handle.publish(b"hello".to_vec()).await?;
    /// ```
    #[must_use]
    pub fn spawn(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        config: Option<SequencerConfig>,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> SequencerHandle {
        let (mut sequencer, handle) = Self::init_with_config(
            channel_id,
            signing_key,
            node_url,
            auth,
            config.unwrap_or_default(),
            checkpoint,
        );
        tokio::spawn(async move {
            loop {
                sequencer.next_event().await;
            }
        });
        handle
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

        eprintln!(
            "[SEQ] next_event called, ready={}, stream={}, backfill={:?}-{:?}, resubmit_active={}, pending={}, tip={:?}",
            self.is_ready(),
            self.blocks_stream.is_some(),
            self.backfill_from,
            self.backfill_to,
            self.resubmit_active,
            self.state.as_ref().map(|s| s.pending_count()).unwrap_or(0),
            self.current_tip.is_some(),
        );
        // Process incremental backfill — one batch per next_event() call.
        // This emits FinalizedInscriptions events for catch-up from checkpoint.
        if let (Some(from), Some(to)) = (self.backfill_from, self.backfill_to) {
            let from_u64: u64 = from.into();
            let to_u64: u64 = to.into();
            if from_u64 <= to_u64 {
                let batch_end = (from_u64 + BACKFILL_BATCH_SIZE).min(to_u64);
                let inscriptions = backfill_batch(
                    self.state.as_mut().unwrap(),
                    from_u64,
                    batch_end,
                    self.channel_id,
                    &self.http_client,
                    &self.node_url,
                )
                .await;

                // Advance cursor
                self.backfill_from = Some(Slot::from(batch_end + 1));

                // Update last_msg_id and finalized_msg from backfilled
                // finalized inscriptions. These are ground truth from
                // the finalized history.
                if let Some(last) = inscriptions.last() {
                    self.last_msg_id = last.this_msg;
                    if let Some(s) = self.state.as_mut() {
                        s.set_finalized_msg(last.this_msg);
                    }
                }

                if !inscriptions.is_empty() {
                    let event = Event::FinalizedInscriptions { inscriptions };
                    drop(self.event_tx.send(event.clone()));
                    return Some(event);
                }
                // Empty batch — return and let next next_event() call
                // process the next batch with the advanced cursor.
                return None;
            }
            // from > to — backfill complete
            self.backfill_from = None;
            self.backfill_to = None;
        }

        // If no blocks stream, try to connect.
        // Requests are not processed until connected — this ensures
        // publish() cannot run before state is initialized and backfill
        // has a chance to start.
        if self.blocks_stream.is_none() {
            // Initialize state from consensus info if needed
            if self.state.is_none() {
                match self.http_client.consensus_info(self.node_url.clone()).await {
                    Ok(info) => {
                        info!(
                            "Sequencer connected: tip={:?}, lib={:?}",
                            info.tip, info.lib
                        );
                        self.state = Some(TxState::new(info.lib, MsgId::root()));
                        self.current_tip = Some(info.tip);
                        // Do NOT update lib_slot here for fresh starts.
                        // Keep it at genesis so the backfill check detects
                        // the gap and catches up on existing inscriptions.
                    }
                    Err(e) => {
                        warn!("Failed to fetch consensus info: {e}");
                        tokio::time::sleep(self.config.reconnect_delay).await;
                        return None;
                    }
                }
            }

            match self
                .http_client
                .get_blocks_stream(self.node_url.clone())
                .await
            {
                Ok(stream) => {
                    self.blocks_stream = Some(Box::pin(stream));
                }
                Err(e) => {
                    warn!("Failed to connect to blocks stream: {e}");
                    tokio::time::sleep(self.config.reconnect_delay).await;
                    return None;
                }
            }

            // Check if we need incremental backfill from checkpoint to
            // current network LIB. Fetch the network's current consensus
            // state to compare against our local lib_slot.
            if self.state.is_some() && self.backfill_from.is_none() {
                match self.http_client.consensus_info(self.node_url.clone()).await {
                    Ok(info) => {
                        let network_lib_slot =
                            get_lib_slot(&self.http_client, &self.node_url, info.lib).await;
                        let from: u64 = self.lib_slot.into();
                        let to: u64 = network_lib_slot.into();
                        if from < to {
                            eprintln!(
                                "[SEQ] Starting incremental backfill from slot {from} to {to}"
                            );
                            self.backfill_from = Some(Slot::from(from + 1));
                            self.backfill_to = Some(network_lib_slot);
                            self.lib_slot = network_lib_slot;
                            return None;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to fetch consensus info for backfill check: {e}");
                    }
                }
            }
        }

        let stream = self.blocks_stream.as_mut()?;

        tokio::select! {
            Some(request) = self.request_rx.recv() => {
                self.handle_request(request);
                None
            }
            maybe_event = stream.next() => {
                if let Some(ref block_event) = maybe_event {
                    eprintln!(
                        "[SEQ] Block event: tip={:?}, old_tip={:?}, last_msg_id={:?}",
                        block_event.tip,
                        self.current_tip,
                        self.last_msg_id,
                    );

                    let result = handle_block_event(
                        block_event,
                        &mut self.state,
                        &mut self.current_tip,
                        &mut self.lib_slot,
                        self.channel_id,
                        &self.http_client,
                        &self.node_url,
                    )
                    .await;

                    eprintln!(
                        "[SEQ] Block processed: last_msg_id={:?}, channel_update={}, finalized={}",
                        self.last_msg_id,
                        result.channel_update.is_some(),
                        result.newly_finalized.len(),
                    );

                    // Signal readiness only when:
                    // - first block event processed (channel discovery done)
                    // - no pending startup backfill
                    if !self.is_ready()
                        && self.backfill_from.is_none()
                        && self.backfill_to.is_none()
                    {
                        let _ = self.ready_tx.send(true);
                    }

                    // Handle channel update.
                    // Update local publish head based on whether we have
                    // pending inscriptions:
                    // - No pending inscriptions: always follow on-chain tip
                    //   (handles clean start on existing channel)
                    // - Has pending, invalidated: rewind to on-chain tip
                    //   and remove invalidated txs
                    // - Has pending, not invalidated: keep local head
                    //   (our pending chain still extends the on-chain tip)
                    if let Some(ref update) = result.channel_update {
                        eprintln!(
                            "[SEQ] ChannelUpdate: invalidated={}, adopted={}, new_tip={:?}",
                            update.invalidated.len(),
                            update.adopted.len(),
                            update.new_channel_tip,
                        );
                        let has_pending_inscriptions = self
                            .state
                            .as_ref()
                            .is_some_and(|s| s.has_pending_inscriptions());

                        if !update.invalidated.is_empty() {
                            // Conflict — rewind and remove invalidated
                            self.last_msg_id = update.new_channel_tip;
                            if let Some(s) = self.state.as_mut() {
                                for inv in &update.invalidated {
                                    eprintln!("[SEQ] Invalidated: tx={:?}, payload={:?}", inv.tx_hash, String::from_utf8_lossy(&inv.payload));
                                    s.remove_pending(&inv.tx_hash);
                                }
                            }
                        } else if !has_pending_inscriptions {
                            // No pending chain — follow on-chain tip
                            self.last_msg_id = update.new_channel_tip;
                        }
                        // Otherwise: extension with valid pending chain,
                        // keep local publish head
                    }

                    // Emit both channel update and finalization events.
                    // If both occur on the same block, return one now and
                    // buffer the other for the next next_event() call.
                    let channel_event = result.channel_update.map(|update| {
                        Event::ChannelUpdate {
                            invalidated: update.invalidated,
                            adopted: update.adopted,
                            new_channel_tip: update.new_channel_tip,
                        }
                    });

                    let finalized_event = if result.newly_finalized.is_empty() {
                        None
                    } else {
                        Some(Event::TxsFinalized {
                            tx_hashes: result.newly_finalized,
                        })
                    };

                    match (channel_event, finalized_event) {
                        (Some(ce), Some(fe)) => {
                            // Both events — return channel update, buffer finalization
                            self.buffered_event = Some(fe);
                            drop(self.event_tx.send(ce.clone()));
                            Some(ce)
                        }
                        (Some(ce), None) => {
                            drop(self.event_tx.send(ce.clone()));
                            Some(ce)
                        }
                        (None, Some(fe)) => {
                            drop(self.event_tx.send(fe.clone()));
                            Some(fe)
                        }
                        (None, None) => None,
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
            _ = self.resubmit_interval.tick(), if !self.resubmit_active && self.state.is_some() && self.current_tip.is_some() => {
                let pending_count = self.state.as_ref().unwrap().pending_count();
                eprintln!("[SEQ] Resubmit tick: pending={pending_count}, tip={:?}", self.current_tip);
                enqueue_resubmit(
                    self.state.as_ref().unwrap(),
                    self.current_tip.unwrap(),
                    &self.http_client,
                    &self.node_url,
                    &self.in_flight,
                    &mut self.resubmit_active,
                );
                None
            }
        }
    }

    fn handle_request(&mut self, request: ActorRequest) {
        if !self.is_ready() {
            match request {
                ActorRequest::Publish { reply, .. } => {
                    drop(reply.send(Err(Error::Unavailable {
                        reason: "sequencer not yet ready",
                    })));
                }
                ActorRequest::SetKeys { reply, .. } => {
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
            ActorRequest::Publish { data, reply } => {
                // Derive publish parent from state instead of trusting
                // last_msg_id blindly — handles branch switches correctly.
                let parent = if let Some(tip) = self.current_tip {
                    s.publish_parent(tip)
                } else {
                    self.last_msg_id
                };
                eprintln!("[SEQ] Publishing with parent={parent:?}");
                let (signed_tx, new_msg_id) =
                    create_inscribe_tx(self.channel_id, &self.signing_key, data.clone(), parent);
                let id = signed_tx.mantle_tx.hash();

                s.submit_inscription(id, signed_tx.clone(), parent, new_msg_id, data);
                self.last_msg_id = new_msg_id;

                let checkpoint = build_checkpoint(s, self.last_msg_id, self.lib_slot);
                let result = PublishResult {
                    inscription_id: id,
                    checkpoint,
                };
                drop(reply.send(Ok((signed_tx, result))));
            }
            ActorRequest::SetKeys { keys, reply } => {
                let signed_tx = create_set_keys_tx(self.channel_id, &self.signing_key, keys);
                let id = signed_tx.mantle_tx.hash();
                s.submit_other(id, signed_tx.clone());
                drop(reply.send(Ok(signed_tx)));
            }
        }
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
    newly_finalized: Vec<TxHash>,
    channel_update: Option<crate::state::ChannelUpdateInfo>,
}

/// Process a block event. Returns finalized tx hashes and optional channel
/// update.
async fn handle_block_event(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) -> BlockEventResult {
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
            newly_finalized: Vec::new(),
            channel_update: None,
        };
    };

    let old_tip = *current_tip;

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind)
    eprintln!(
        "[SEQ] LIB check: event_lib={lib:?}, state_lib={:?}, equal={}",
        s.lib(),
        lib == s.lib()
    );
    let mut lib_finalized = Vec::new();
    if lib != s.lib() {
        let new_lib_slot = get_lib_slot(http_client, node_url, lib).await;
        if *lib_slot < new_lib_slot {
            lib_finalized = backfill_to_lib(
                s,
                *lib_slot,
                new_lib_slot,
                channel_id,
                http_client,
                node_url,
            )
            .await;
        }
        *lib_slot = new_lib_slot;
    }

    // 2. Backfill canonical chain if parent is missing
    if !s.has_block(&parent_id) && parent_id != s.lib() {
        backfill_canonical(s, parent_id, channel_id, http_client, node_url).await;
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
    let mut newly_finalized = s.process_block(block_id, parent_id, lib, our_txs, inscriptions);

    // Finalize txs found in backfilled LIB blocks — this is ground truth
    // from the node. LIB blocks are truly final (can't be reorged), so
    // txs found there are definitively on the canonical chain. Our safe
    // set may miss them due to gaps or reorgs in the block event stream.
    for tx_hash in lib_finalized {
        if s.remove_pending(&tx_hash).is_some() {
            eprintln!("[SEQ] Backfill-finalized tx={tx_hash:?}");
            newly_finalized.push(tx_hash);
        }
    }
    *current_tip = Some(tip);

    // Detect channel changes.
    // On first event (old_tip is None), check for existing inscriptions on
    // the channel — this handles clean start on an existing channel.
    // On subsequent events, detect channel update if tip changed.
    let channel_update = match old_tip {
        Some(old) if old != tip => s.detect_channel_update(old, tip),
        None => {
            // First event — check if the channel already has inscriptions.
            // Treat as a reorg from root: the LCM is finalized_msg, and
            // any pending inscriptions chaining from it are orphaned.
            let channel_tip = s.channel_tip_at(tip);
            if channel_tip != MsgId::root() {
                let adopted = s.collect_inscriptions_on_branch(tip);
                let invalidated = s.collect_pending_suffix(s.finalized_msg());
                if !adopted.is_empty() || !invalidated.is_empty() {
                    Some(crate::state::ChannelUpdateInfo {
                        invalidated,
                        adopted,
                        new_channel_tip: channel_tip,
                    })
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None, // tip unchanged
    };

    BlockEventResult {
        newly_finalized,
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

async fn get_lib_slot(http_client: &CommonHttpClient, node_url: &Url, lib: HeaderId) -> Slot {
    // Try to get the block to find its slot
    match http_client.get_block(node_url.clone(), lib).await {
        Ok(Some(block)) => block.header().slot(),
        Ok(None) => {
            // Genesis case - slot 0
            Slot::genesis()
        }
        Err(e) => {
            warn!("Failed to get lib block slot: {e}, assuming slot 0");
            Slot::genesis()
        }
    }
}

/// Backfill finalized blocks from current `lib_slot` to new `lib_slot`.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
/// Process one batch of backfill blocks and return discovered inscriptions.
async fn backfill_batch(
    state: &mut TxState,
    from_slot: u64,
    to_slot: u64,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) -> Vec<InscriptionInfo> {
    eprintln!("[SEQ] Backfill batch: slots {} to {}", from_slot, to_slot);

    match http_client
        .get_blocks(node_url.clone(), from_slot, to_slot)
        .await
    {
        Ok(blocks) => {
            eprintln!("[SEQ] Backfill got {} blocks", blocks.len());
            let mut batch_inscriptions = Vec::new();
            for block in blocks {
                let block_id = block.header.id;
                let parent_id = block.header.parent_block;
                let slot: u64 = block.header.slot.into();

                let our_txs: Vec<TxHash> = block
                    .transactions
                    .iter()
                    .filter(|tx| matches_channel(tx, channel_id))
                    .map(|tx| tx.mantle_tx.hash())
                    .collect();

                let inscriptions = extract_inscriptions(&block.transactions, channel_id);
                if !inscriptions.is_empty() {
                    eprintln!(
                        "[SEQ] Backfill slot {}: found {} inscriptions, txs={}",
                        slot,
                        inscriptions.len(),
                        our_txs.len(),
                    );
                    for insc in &inscriptions {
                        eprintln!(
                            "[SEQ]   inscription: parent={:?}, this={:?}",
                            insc.parent_msg, insc.this_msg,
                        );
                    }
                }
                batch_inscriptions.extend(inscriptions.clone());

                let current_lib = state.lib();
                state.process_block(block_id, parent_id, current_lib, our_txs, inscriptions);
            }
            eprintln!(
                "[SEQ] Backfill batch done: {} inscriptions found",
                batch_inscriptions.len()
            );
            batch_inscriptions
        }
        Err(e) => {
            warn!("Failed to fetch backfill batch: {e}");
            Vec::new()
        }
    }
}

/// Returns tx hashes of our channel's txs found in finalized blocks.
async fn backfill_to_lib(
    state: &mut TxState,
    from_slot: Slot,
    to_slot: Slot,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) -> Vec<TxHash> {
    let from: u64 = from_slot.into();
    let to: u64 = to_slot.into();

    if from >= to {
        return Vec::new();
    }

    debug!(
        "Backfilling finalized blocks from slot {} to {}",
        from + 1,
        to
    );

    let mut finalized_txs = Vec::new();

    match http_client.get_blocks(node_url.clone(), from + 1, to).await {
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

                finalized_txs.extend(our_txs.iter().copied());

                let inscriptions = extract_inscriptions(&block.transactions, channel_id);

                // Use current state lib to avoid premature finalization
                let current_lib = state.lib();
                state.process_block(block_id, parent_id, current_lib, our_txs, inscriptions);
            }
            debug!("Backfilled {} finalized blocks", to - from);
        }
        Err(e) => {
            warn!("Failed to backfill finalized blocks: {e}");
        }
    }

    finalized_txs
}

/// Backfill canonical chain backwards from a missing parent to LIB.
///
/// Uses `state.lib()` during replay to avoid premature finalization.
/// The caller is responsible for triggering finalization after backfill
/// completes.
async fn backfill_canonical(
    state: &mut TxState,
    missing_parent: HeaderId,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) {
    debug!("Backfilling canonical chain from {:?}", missing_parent);

    let mut blocks_to_process = Vec::new();
    let mut current = missing_parent;
    let lib = state.lib();

    // Walk backwards until we find a known block or reach lib
    while !state.has_block(&current) && current != lib {
        match http_client.get_block(node_url.clone(), current).await {
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

fn enqueue_resubmit(
    state: &TxState,
    tip: HeaderId,
    http_client: &CommonHttpClient,
    node_url: &Url,
    in_flight: &FuturesUnordered<BoxFuture<'static, InFlight>>,
    resubmit_active: &mut bool,
) {
    let pending: Vec<(InscriptionId, SignedMantleTx)> = state.pending_txs(tip);

    if pending.is_empty() {
        return;
    }

    debug!("Resubmitting {} pending inscription(s)", pending.len());

    let client = http_client.clone();
    let url = node_url.clone();
    *resubmit_active = true;

    in_flight.push(Box::pin(async move {
        let mut results = Vec::with_capacity(pending.len());
        for (id, tx) in pending {
            let result = client
                .post_transaction(url.clone(), tx)
                .await
                .map_err(|e| e.to_string());
            results.push((id, result));
        }
        InFlight::ResubmittedBatch { results }
    }));
}

/// Extract channel inscription info from a block's transactions.
fn extract_inscriptions(txs: &[SignedMantleTx], channel_id: ChannelId) -> Vec<InscriptionInfo> {
    txs.iter()
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
        .collect()
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

    let inscribe_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscribe_op)],
        storage_gas_price: 0,
        execution_gas_price: 0,
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

    let set_keys_tx = MantleTx {
        ops: vec![Op::ChannelSetKeys(set_keys_op)],
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = set_keys_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

    SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        mantle_tx: set_keys_tx,
    }
}
