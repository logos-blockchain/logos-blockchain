use std::time::Duration;

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient, ProcessedBlockEvent, Slot};
use lb_core::{
    header::HeaderId,
    mantle::{
        MantleTx, SignedMantleTx, Transaction as _,
        ledger::Tx as LedgerTx,
        ops::{
            Op, OpProof,
            channel::{ChannelId, MsgId, inscribe::InscriptionOp},
        },
        tx::TxHash,
    },
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use reqwest::Url;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::state::{TxState, TxStatus};

const DEFAULT_RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const DEFAULT_PUBLISH_CHANNEL_CAPACITY: usize = 256;

/// Inscription identifier.
pub type InscriptionId = TxHash;

/// Inscription status.
pub type InscriptionStatus = TxStatus;

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
}

enum ActorRequest {
    Publish {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(SignedMantleTx, PublishResult), Error>>,
    },
    Status {
        id: InscriptionId,
        reply: oneshot::Sender<Result<TxStatus, Error>>,
    },
    Checkpoint {
        reply: oneshot::Sender<Result<SequencerCheckpoint, Error>>,
    },
}

enum InFlight {
    ResubmittedBatch {
        results: Vec<(InscriptionId, Result<(), String>)>,
    },
}

/// Zone sequencer client.
pub struct ZoneSequencer {
    request_tx: mpsc::Sender<ActorRequest>,
    node_url: Url,
    http_client: CommonHttpClient,
}

impl ZoneSequencer {
    #[must_use]
    pub fn init(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> Self {
        Self::init_with_config(
            channel_id,
            signing_key,
            node_url,
            auth,
            SequencerConfig::default(),
            checkpoint,
        )
    }

    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        config: SequencerConfig,
        checkpoint: Option<SequencerCheckpoint>,
    ) -> Self {
        let http_client = CommonHttpClient::new(auth);
        let (request_tx, request_rx) = mpsc::channel(config.publish_channel_capacity);

        tokio::spawn(run_loop(
            request_rx,
            channel_id,
            signing_key,
            node_url.clone(),
            http_client.clone(),
            config,
            checkpoint,
        ));

        Self {
            request_tx,
            node_url,
            http_client,
        }
    }

    /// Publish an inscription to the zone's channel.
    ///
    /// Returns the inscription ID and a checkpoint for persistence.
    pub async fn publish(&self, data: Vec<u8>) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = ActorRequest::Publish {
            data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let (signed_tx, result) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
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

    /// Get the status of an inscription.
    pub async fn status(&self, id: InscriptionId) -> Result<InscriptionStatus, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = ActorRequest::Status {
            id,
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

    /// Get the current checkpoint for persistence.
    pub async fn checkpoint(&self) -> Result<SequencerCheckpoint, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = ActorRequest::Checkpoint { reply: reply_tx };

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
}

async fn initialize_from_checkpoint(
    http_client: &CommonHttpClient,
    node_url: &Url,
    reconnect_delay: Duration,
    checkpoint: Option<SequencerCheckpoint>,
) -> (TxState, HeaderId, Slot, MsgId) {
    // Get current network state
    let info = loop {
        match http_client.consensus_info(node_url.clone()).await {
            Ok(info) => {
                info!(
                    "Sequencer connected: tip={:?}, lib={:?}",
                    info.tip, info.lib
                );
                break info;
            }
            Err(e) => {
                warn!(
                    "Failed to fetch consensus info: {e}, retrying in {:?}",
                    reconnect_delay
                );
                tokio::time::sleep(reconnect_delay).await;
            }
        }
    };

    if let Some(cp) = checkpoint {
        info!(
            "Restoring from checkpoint: {} pending txs, lib={:?}, lib_slot={:?}",
            cp.pending_txs.len(),
            cp.lib,
            cp.lib_slot
        );
        let mut state = TxState::new(cp.lib);
        // Restore pending transactions
        for (hash, tx) in cp.pending_txs {
            state.submit(hash, tx);
        }
        // Use checkpoint's lib_slot as starting point for backfill
        (state, info.tip, cp.lib_slot, cp.last_msg_id)
    } else {
        // Fresh start: get lib slot from network
        let lib_slot = get_lib_slot(http_client, node_url, info.lib).await;
        info!("Starting fresh (no checkpoint)");
        (TxState::new(info.lib), info.tip, lib_slot, MsgId::root())
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

async fn connect_blocks_stream(
    http_client: &CommonHttpClient,
    node_url: &Url,
    reconnect_delay: Duration,
) -> impl futures::Stream<Item = ProcessedBlockEvent> {
    loop {
        match http_client.get_blocks_stream(node_url.clone()).await {
            Ok(stream) => return stream,
            Err(e) => {
                warn!(
                    "Failed to connect to blocks stream: {e}, retrying in {:?}",
                    reconnect_delay
                );
                tokio::time::sleep(reconnect_delay).await;
            }
        }
    }
}

async fn run_loop(
    mut request_rx: mpsc::Receiver<ActorRequest>,
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node_url: Url,
    http_client: CommonHttpClient,
    config: SequencerConfig,
    checkpoint: Option<SequencerCheckpoint>,
) {
    let (state, current_tip, lib_slot, last_msg_id) =
        initialize_from_checkpoint(&http_client, &node_url, config.reconnect_delay, checkpoint)
            .await;
    let mut state = Some(state);
    let mut current_tip = Some(current_tip);
    let mut lib_slot = lib_slot;
    let mut last_msg_id = last_msg_id;

    let mut resubmit_interval = tokio::time::interval(config.resubmit_interval);
    let mut resubmit_active = false;
    let mut in_flight: FuturesUnordered<BoxFuture<'static, InFlight>> = FuturesUnordered::new();

    loop {
        let blocks_stream =
            connect_blocks_stream(&http_client, &node_url, config.reconnect_delay).await;
        tokio::pin!(blocks_stream);

        loop {
            tokio::select! {
                Some(request) = request_rx.recv() => {
                    handle_request(
                        request,
                        &mut state,
                        current_tip,
                        lib_slot,
                        channel_id,
                        &signing_key,
                        &mut last_msg_id,
                    );
                }
                maybe_event = blocks_stream.next() => {
                    if let Some(ref event) = maybe_event {
                        handle_block_event(
                            event,
                            &mut state,
                            &mut current_tip,
                            &mut lib_slot,
                            channel_id,
                            &http_client,
                            &node_url,
                        )
                        .await;
                    } else {
                        warn!("Blocks stream disconnected, reconnecting...");
                        break;
                    }
                }
                Some(event) = in_flight.next(), if !in_flight.is_empty() => {
                    handle_inflight(event, &mut resubmit_active);
                }
                _ = resubmit_interval.tick(), if !resubmit_active && state.is_some() && current_tip.is_some() => {
                    enqueue_resubmit(
                        state.as_ref().unwrap(),
                        current_tip.unwrap(),
                        &http_client,
                        &node_url,
                        &in_flight,
                        &mut resubmit_active,
                    );
                }
            }
        }
    }
}

fn handle_request(
    request: ActorRequest,
    state: &mut Option<TxState>,
    current_tip: Option<HeaderId>,
    lib_slot: Slot,
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    last_msg_id: &mut MsgId,
) {
    let Some(s) = state else {
        match request {
            ActorRequest::Publish { reply, .. } => {
                drop(reply.send(Err(Error::Unavailable {
                    reason: "not initialized",
                })));
            }
            ActorRequest::Status { reply, .. } => {
                drop(reply.send(Err(Error::Unavailable {
                    reason: "not initialized",
                })));
            }
            ActorRequest::Checkpoint { reply } => {
                drop(reply.send(Err(Error::Unavailable {
                    reason: "not initialized",
                })));
            }
        }
        return;
    };

    match request {
        ActorRequest::Publish { data, reply } => {
            let (signed_tx, new_msg_id) =
                create_inscribe_tx(channel_id, signing_key, data, *last_msg_id);
            let id = signed_tx.mantle_tx.hash();

            s.submit(id, signed_tx.clone());
            *last_msg_id = new_msg_id;

            let checkpoint = build_checkpoint(s, *last_msg_id, lib_slot);
            let result = PublishResult {
                inscription_id: id,
                checkpoint,
            };
            drop(reply.send(Ok((signed_tx, result))));
        }
        ActorRequest::Status { id, reply } => {
            let result = current_tip.map_or(
                Err(Error::Unavailable {
                    reason: "not synced (no tip yet)",
                }),
                |tip| Ok(s.status(&id, tip)),
            );
            drop(reply.send(result));
        }
        ActorRequest::Checkpoint { reply } => {
            let checkpoint = build_checkpoint(s, *last_msg_id, lib_slot);
            drop(reply.send(Ok(checkpoint)));
        }
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

async fn handle_block_event(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    lib_slot: &mut Slot,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) {
    let block_id = event.block.header.id;
    let parent_id = event.block.header.parent_block;
    let tip = event.tip;
    let lib = event.lib;

    // Initialize state on first event
    if state.is_none() {
        *state = Some(TxState::new(lib));
    }

    let Some(s) = state.as_mut() else {
        return;
    };

    // Backfill if needed (self-healing on every event)
    // 1. Backfill finalized blocks up to LIB (only when state's LIB is behind)
    if lib != s.lib() {
        let new_lib_slot = get_lib_slot(http_client, node_url, lib).await;
        if *lib_slot < new_lib_slot {
            backfill_to_lib(
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

    // Extract tx hashes for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    // Process the actual event block with real lib (triggers finalization if lib
    // advanced)
    s.process_block(block_id, parent_id, lib, our_txs);
    *current_tip = Some(tip);
}

fn handle_inflight(event: InFlight, resubmit_active: &mut bool) {
    match event {
        InFlight::ResubmittedBatch { results } => {
            for (id, result) in results {
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
async fn backfill_to_lib(
    state: &mut TxState,
    from_slot: Slot,
    to_slot: Slot,
    channel_id: ChannelId,
    http_client: &CommonHttpClient,
    node_url: &Url,
) {
    let from: u64 = from_slot.into();
    let to: u64 = to_slot.into();

    if from >= to {
        return; // No-op
    }

    debug!(
        "Backfilling finalized blocks from slot {} to {}",
        from + 1,
        to
    );

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

                // Use current state lib to avoid premature finalization
                let current_lib = state.lib();
                state.process_block(block_id, parent_id, current_lib, our_txs);
            }
            debug!("Backfilled {} finalized blocks", to - from);
        }
        Err(e) => {
            warn!("Failed to backfill finalized blocks: {e}");
        }
    }
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

        let our_txs: Vec<TxHash> = block
            .transactions()
            .filter(|tx| matches_channel(tx, channel_id))
            .map(|tx| tx.mantle_tx.hash())
            .collect();

        // Use current state lib to avoid premature finalization
        state.process_block(block_id, parent_id, lib, our_txs);
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
    let pending: Vec<(InscriptionId, SignedMantleTx)> = state
        .pending_txs(tip)
        .map(|(hash, tx)| (*hash, tx.clone()))
        .collect();

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

fn matches_channel(tx: &SignedMantleTx, channel_id: ChannelId) -> bool {
    tx.mantle_tx
        .ops
        .iter()
        .any(|op| matches!(op, Op::ChannelInscribe(inscribe) if inscribe.channel_id == channel_id))
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

    let ledger_tx = LedgerTx::new(vec![], vec![]);

    let inscribe_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(inscribe_op)],
        ledger_tx,
        storage_gas_price: 0,
        execution_gas_price: 0,
    };

    let tx_hash = inscribe_tx.hash();
    let signature = signing_key.sign_payload(tx_hash.as_signing_bytes().as_ref());

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        ledger_tx_proof: ZkKey::multi_sign(&[], tx_hash.as_ref())
            .expect("multi-sign with empty key set"),
        mantle_tx: inscribe_tx,
    };

    (signed_tx, msg_id)
}
