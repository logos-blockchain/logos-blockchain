use std::time::Duration;

use futures::{StreamExt as _, future::BoxFuture, stream::FuturesUnordered};
use lb_common_http_client::{BasicAuthCredentials, CommonHttpClient};
use lb_core::mantle::{
    MantleTx, SignedMantleTx, Transaction as _,
    ledger::Tx as LedgerTx,
    ops::{
        Op, OpProof,
        channel::{ChannelId, Ed25519PublicKey, MsgId, inscribe::InscriptionOp},
    },
    tx::TxHash,
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkKey};
use reqwest::Url;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::state::State;

const RESUBMIT_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const PUBLISH_CHANNEL_CAPACITY: usize = 256;

/// Result of a `publish_block` call.
pub struct PublishResult {
    pub tx_hash: TxHash,
}

/// Sequencer errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sequencer unavailable: {reason}")]
    Unavailable { reason: &'static str },
}

struct PublishRequest {
    data: Vec<u8>,
    reply: oneshot::Sender<(SignedMantleTx, TxHash)>,
}

/// Completion events from background async work.
enum InFlight {
    FetchedLibBlock {
        header_id: lb_core::header::HeaderId,
        block: Option<Box<lb_core::block::Block<SignedMantleTx>>>,
    },
    ResubmittedBatch {
        results: Vec<(TxHash, Result<(), String>)>,
    },
}

/// Zone sequencer client.
///
/// Creates zone blocks as channel inscriptions, tracks them as pending,
/// and submits them to the mempool. Pending transactions are resubmitted
/// periodically in the background and removed when finalized in LIB.
pub struct ZoneSequencer {
    request_tx: mpsc::Sender<PublishRequest>,
    node_url: Url,
    http_client: CommonHttpClient,
}

impl ZoneSequencer {
    /// Initialize a new sequencer.
    ///
    /// Spawns a background actor that owns all mutable state, connects to
    /// the node's LIB stream, tracks finalized transactions, and resubmits
    /// pending transactions periodically.
    #[must_use]
    pub fn init(
        channel_id: ChannelId,
        signing_key: Ed25519Key,
        node_url: Url,
        auth: Option<BasicAuthCredentials>,
    ) -> Self {
        let http_client = CommonHttpClient::new(auth);
        let (request_tx, request_rx) = mpsc::channel(PUBLISH_CHANNEL_CAPACITY);

        tokio::spawn(run_loop(
            request_rx,
            channel_id,
            signing_key,
            node_url.clone(),
            http_client.clone(),
        ));

        Self {
            request_tx,
            node_url,
            http_client,
        }
    }

    /// Publish a zone block.
    ///
    /// Sends the data to the background actor which creates the inscription
    /// transaction and tracks it as pending. Then posts the transaction to
    /// the mempool inline. If the post fails the transaction remains pending
    /// and will be retried by the periodic resubmit.
    pub async fn publish_block(&self, data: Vec<u8>) -> Result<PublishResult, Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = PublishRequest {
            data,
            reply: reply_tx,
        };

        self.request_tx
            .send(request)
            .await
            .map_err(|_| Error::Unavailable {
                reason: "actor channel closed",
            })?;

        let (signed_tx, tx_hash) = reply_rx.await.map_err(|_| Error::Unavailable {
            reason: "actor dropped reply",
        })?;

        info!("Created inscription tx {tx_hash:?}");

        if let Err(e) = self
            .http_client
            .post_transaction(self.node_url.clone(), signed_tx)
            .await
        {
            warn!("Failed to post transaction: {e}");
        }

        Ok(PublishResult { tx_hash })
    }
}

async fn run_loop(
    mut request_rx: mpsc::Receiver<PublishRequest>,
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    node_url: Url,
    http_client: CommonHttpClient,
) {
    let mut state = State::new();
    let mut last_msg_id = MsgId::root();
    let mut resubmit_interval = tokio::time::interval(RESUBMIT_INTERVAL);
    let mut resubmit_active = false;
    let mut in_flight: FuturesUnordered<BoxFuture<'static, InFlight>> = FuturesUnordered::new();

    loop {
        let lib_stream = match http_client.get_lib_stream(node_url.clone()).await {
            Ok(stream) => stream,
            Err(e) => {
                warn!("Failed to connect to LIB stream: {e}, retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };

        tokio::pin!(lib_stream);

        loop {
            tokio::select! {
                Some(request) = request_rx.recv() => {
                    let (signed_tx, new_msg_id) =
                        create_inscribe_tx(channel_id, &signing_key, request.data, last_msg_id);
                    let tx_hash = signed_tx.mantle_tx.hash();

                    state.submit_pending(tx_hash, signed_tx.clone());
                    last_msg_id = new_msg_id;

                    drop(request.reply.send((signed_tx, tx_hash)));
                }
                maybe_block = lib_stream.next() => {
                    if let Some(block_info) = maybe_block {
                        let client = http_client.clone();
                        let url = node_url.clone();
                        let header_id = block_info.header_id;
                        in_flight.push(Box::pin(async move {
                            let block = fetch_block(&client, &url, header_id).await.map(Box::new);
                            InFlight::FetchedLibBlock { header_id, block }
                        }));
                    } else {
                        warn!("LIB stream disconnected, reconnecting...");
                        break;
                    }
                }
                Some(event) = in_flight.next(), if !in_flight.is_empty() => {
                    handle_completion(&mut state, &mut resubmit_active, event);
                }
                _ = resubmit_interval.tick(), if !resubmit_active => {
                    enqueue_resubmit(&state, &http_client, &node_url, &in_flight, &mut resubmit_active);
                }
            }
        }
    }
}

fn handle_completion(state: &mut State, resubmit_active: &mut bool, event: InFlight) {
    match event {
        InFlight::FetchedLibBlock { header_id, block } => {
            if let Some(block) = block {
                let finalized = state.process_lib_block(block.transactions_vec());
                for tx_hash in &finalized {
                    info!("Finalized tx {tx_hash:?} in LIB block {header_id}");
                }
            }
        }
        InFlight::ResubmittedBatch { results } => {
            for (tx_hash, result) in results {
                if let Err(e) = result {
                    warn!("Failed to resubmit pending tx {tx_hash:?}: {e}");
                }
            }
            *resubmit_active = false;
        }
    }
}

fn enqueue_resubmit(
    state: &State,
    http_client: &CommonHttpClient,
    node_url: &Url,
    in_flight: &FuturesUnordered<BoxFuture<'static, InFlight>>,
    resubmit_active: &mut bool,
) {
    let pending: Vec<(TxHash, SignedMantleTx)> = state
        .pending_txs()
        .map(|(hash, tx)| (*hash, tx.clone()))
        .collect();

    if pending.is_empty() {
        return;
    }

    debug!("Resubmitting {} pending tx(s)", pending.len());

    let client = http_client.clone();
    let url = node_url.clone();
    *resubmit_active = true;

    in_flight.push(Box::pin(async move {
        let mut results = Vec::with_capacity(pending.len());
        for (tx_hash, tx) in pending {
            let result = client
                .post_transaction(url.clone(), tx)
                .await
                .map_err(|e| e.to_string());
            results.push((tx_hash, result));
        }
        InFlight::ResubmittedBatch { results }
    }));
}

async fn fetch_block(
    http_client: &CommonHttpClient,
    node_url: &Url,
    header_id: lb_core::header::HeaderId,
) -> Option<lb_core::block::Block<SignedMantleTx>> {
    match http_client.get_block(node_url.clone(), header_id).await {
        Ok(Some(block)) => Some(block),
        Ok(None) => {
            warn!("LIB block {header_id} not found in storage");
            None
        }
        Err(e) => {
            warn!("Failed to fetch LIB block {header_id}: {e}");
            None
        }
    }
}

fn create_inscribe_tx(
    channel_id: ChannelId,
    signing_key: &Ed25519Key,
    data: Vec<u8>,
    parent: MsgId,
) -> (SignedMantleTx, MsgId) {
    let verifying_key_bytes = signing_key.public_key().to_bytes();
    let verifying_key =
        Ed25519PublicKey::from_bytes(&verifying_key_bytes).expect("valid ed25519 public key");

    let inscribe_op = InscriptionOp {
        channel_id,
        inscription: data,
        parent,
        signer: verifying_key,
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
    let signature_bytes = signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref())
        .to_bytes();
    let signature =
        lb_key_management_system_service::keys::Ed25519Signature::from_bytes(&signature_bytes);

    let signed_tx = SignedMantleTx {
        ops_proofs: vec![OpProof::Ed25519Sig(signature)],
        ledger_tx_proof: ZkKey::multi_sign(&[], tx_hash.as_ref())
            .expect("multi-sign with empty key set"),
        mantle_tx: inscribe_tx,
    };

    (signed_tx, msg_id)
}
