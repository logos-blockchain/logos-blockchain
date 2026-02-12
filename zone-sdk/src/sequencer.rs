use std::time::Duration;

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient, ProcessedBlockEvent};
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

/// Configuration for the zone sequencer.
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
        reply: oneshot::Sender<Result<(SignedMantleTx, InscriptionId), Error>>,
    },
    Status {
        id: InscriptionId,
        reply: oneshot::Sender<Result<TxStatus, Error>>,
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
    ) -> Self {
        Self::init_with_config(
            channel_id,
            signing_key,
            node_url,
            auth,
            SequencerConfig::default(),
        )
    }

    #[must_use]
    pub fn init_with_config(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
        config: SequencerConfig,
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
        ));

        Self {
            request_tx,
            node_url,
            http_client,
        }
    }

    /// Publish an inscription to the zone's channel.
    pub async fn publish(&self, data: Vec<u8>) -> Result<InscriptionId, Error> {
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

        let (signed_tx, id) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })??;

        info!("Created inscription {id:?}");

        if let Err(e) = self
            .http_client
            .post_transaction(self.node_url.clone(), signed_tx)
            .await
        {
            warn!("Failed to post transaction: {e}");
        }

        Ok(id)
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
}

async fn initialize_state(
    http_client: &CommonHttpClient,
    node_url: &Url,
    reconnect_delay: Duration,
) -> (TxState, HeaderId) {
    loop {
        match http_client.consensus_info(node_url.clone()).await {
            Ok(info) => {
                info!(
                    "Sequencer initialized from consensus info: tip={:?}, lib={:?}",
                    info.tip, info.lib
                );
                return (TxState::new(info.lib), info.tip);
            }
            Err(e) => {
                warn!(
                    "Failed to fetch consensus info: {e}, retrying in {:?}",
                    reconnect_delay
                );
                tokio::time::sleep(reconnect_delay).await;
            }
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
) {
    let (state, current_tip) =
        initialize_state(&http_client, &node_url, config.reconnect_delay).await;
    let mut state = Some(state);
    let mut current_tip = Some(current_tip);

    let mut last_msg_id = MsgId::root();
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
                            channel_id,
                        );
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

            drop(reply.send(Ok((signed_tx, id))));
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
    }
}

fn handle_block_event(
    event: &ProcessedBlockEvent,
    state: &mut Option<TxState>,
    current_tip: &mut Option<HeaderId>,
    channel_id: ChannelId,
) {
    let block_id = event.block.header.id;
    let parent_id = event.block.header.parent_block;
    let tip = event.tip;
    let lib = event.lib;

    // Initialize state on first event
    if state.is_none() {
        *state = Some(TxState::new(lib));
    }

    // Extract tx hashes for our channel
    let our_txs: Vec<TxHash> = event
        .block
        .transactions
        .iter()
        .filter(|tx| matches_channel(tx, channel_id))
        .map(|tx| tx.mantle_tx.hash())
        .collect();

    if let Some(s) = state {
        s.process_block(block_id, parent_id, lib, our_txs);
    }

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
