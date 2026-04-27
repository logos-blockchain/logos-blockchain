use std::{
    collections::HashMap,
    marker::PhantomData,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use lb_core::mantle::{
    MantleTx, SignedMantleTx, Transaction as _,
    genesis_tx::GENESIS_STORAGE_GAS_PRICE,
    ops::{
        Op, OpProof,
        channel::{ChannelId, MsgId, inscribe::InscriptionOp},
    },
    tx::TxHash,
};
use lb_key_management_system_service::keys::Ed25519Key;
use rand::{seq::SliceRandom as _, thread_rng};
use testing_framework_core::scenario::{
    DynError, RunContext, RunMetrics, Workload as ScenarioWorkload,
};
use thiserror::Error;
use tokio::{
    sync::broadcast::error::RecvError,
    time::{Instant as TokioInstant, timeout},
};
use tracing::{debug, info, warn};

use crate::{
    framework::{BlockRecord, LbcEnv},
    node::{DeploymentPlan, NodeHttpClient},
    workloads::{BlockFeedSubscription, LbcBlockFeedEnv, LbcScenarioEnv},
};

const BLOCK_POLL_TIMEOUT: Duration = Duration::from_secs(1);
const SUBMIT_RETRIES: usize = 5;
const SUBMIT_RETRY_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_PAYLOAD_BYTES: usize = 128;
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
enum InscriptionWorkloadError {
    #[error("inscription workload requires at least one channel")]
    MissingChannels,
    #[error("cluster client exhausted all nodes")]
    ClusterClientExhausted,
    #[error("block feed subscription closed")]
    FeedClosed,
    #[error("failed to build signed inscription transaction: {0}")]
    SignedTransactionBuild(String),
    #[error("inscription workload confirmed {confirmed} txs; required at least {required}")]
    ConfirmedBelowRequired { confirmed: u64, required: u64 },
}

#[derive(Clone)]
pub struct WorkloadImpl<E = LbcEnv> {
    channel_count: NonZeroUsize,
    payload_bytes: NonZeroUsize,
    min_confirmed: u64,
    _env: PhantomData<fn() -> E>,
}

pub type Workload<E = LbcEnv> = WorkloadImpl<E>;

#[async_trait]
impl<E> ScenarioWorkload<E> for WorkloadImpl<E>
where
    E: LbcScenarioEnv + LbcBlockFeedEnv,
{
    fn name(&self) -> &'static str {
        "inscription_workload"
    }

    fn init(
        &mut self,
        descriptors: &DeploymentPlan,
        _run_metrics: &RunMetrics,
    ) -> Result<(), DynError> {
        let _ = descriptors;
        Ok(())
    }

    async fn start(&self, ctx: &RunContext<E>) -> Result<(), DynError> {
        if self.channel_count.get() == 0 {
            return Err(InscriptionWorkloadError::MissingChannels.into());
        }

        let mut runner = InscriptionRunner::new(self, ctx)?;
        runner.run().await
    }
}

impl<E> WorkloadImpl<E> {
    #[must_use]
    pub fn new(channel_count: NonZeroUsize) -> Self {
        Self {
            channel_count,
            payload_bytes: NonZeroUsize::new(DEFAULT_PAYLOAD_BYTES).expect("constant is non-zero"),
            min_confirmed: 0,
            _env: PhantomData,
        }
    }

    #[must_use]
    pub const fn with_channel_count(mut self, channel_count: NonZeroUsize) -> Self {
        self.channel_count = channel_count;
        self
    }

    #[must_use]
    pub const fn with_payload_bytes(mut self, payload_bytes: NonZeroUsize) -> Self {
        self.payload_bytes = payload_bytes;
        self
    }

    #[must_use]
    pub const fn with_min_confirmed(mut self, min_confirmed: u64) -> Self {
        self.min_confirmed = min_confirmed;
        self
    }
}

impl<E> Default for WorkloadImpl<E> {
    fn default() -> Self {
        Self::new(NonZeroUsize::MIN)
    }
}

struct InscriptionRunner<'a, E: LbcScenarioEnv> {
    channels: Vec<ChannelState>,
    pending_by_hash: HashMap<TxHash, usize>,
    feed: BlockFeedSubscription,
    ctx: &'a RunContext<E>,
    payload_bytes: usize,
    min_confirmed: u64,
    deadline: TokioInstant,
}

struct ChannelState {
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    parent: MsgId,
    next_nonce: u64,
    pending: Option<PendingSubmission>,
    submitted: u64,
    confirmed: u64,
}

struct PendingSubmission {
    tx_hash: TxHash,
    msg_id: MsgId,
    submitted_at: Instant,
}

impl<'a, E: LbcScenarioEnv + LbcBlockFeedEnv> InscriptionRunner<'a, E> {
    fn new(workload: &WorkloadImpl<E>, ctx: &'a RunContext<E>) -> Result<Self, DynError> {
        let channels = build_channel_states(workload.channel_count.get(), &resolve_run_salt());
        if channels.is_empty() {
            return Err(InscriptionWorkloadError::MissingChannels.into());
        }

        Ok(Self {
            channels,
            pending_by_hash: HashMap::new(),
            feed: E::block_feed_subscription(ctx)?,
            ctx,
            payload_bytes: workload.payload_bytes.get(),
            min_confirmed: workload.min_confirmed,
            deadline: TokioInstant::now() + ctx.run_duration(),
        })
    }

    async fn run(&mut self) -> Result<(), DynError> {
        let mut next_progress_log = TokioInstant::now() + PROGRESS_LOG_INTERVAL;

        info!(
            channels = self.channels.len(),
            payload_bytes = self.payload_bytes,
            duration_secs = self.ctx.run_duration().as_secs(),
            "starting inscription workload"
        );

        loop {
            if TokioInstant::now() >= self.deadline {
                break;
            }

            self.submit_ready_channels().await?;
            self.wait_for_block_or_timeout().await?;

            if TokioInstant::now() >= next_progress_log {
                self.log_progress();
                next_progress_log += PROGRESS_LOG_INTERVAL;
            }
        }

        let (submitted, confirmed, pending) = self.stats();

        info!(
            submitted,
            confirmed, pending, "inscription workload finished"
        );

        if confirmed < self.min_confirmed {
            return Err(InscriptionWorkloadError::ConfirmedBelowRequired {
                confirmed,
                required: self.min_confirmed,
            }
            .into());
        }

        Ok(())
    }

    fn log_progress(&self) {
        let (submitted, confirmed, pending) = self.stats();
        info!(
            submitted,
            confirmed, pending, "inscription workload progress"
        );
    }

    async fn submit_ready_channels(&mut self) -> Result<(), DynError> {
        let ready_channels = self
            .channels
            .iter()
            .enumerate()
            .filter_map(|(idx, channel)| channel.pending.is_none().then_some(idx))
            .collect::<Vec<_>>();

        for channel_idx in ready_channels {
            self.submit_next(channel_idx).await?;
        }

        Ok(())
    }

    async fn submit_next(&mut self, channel_idx: usize) -> Result<(), DynError> {
        let Some(channel) = self.channels.get_mut(channel_idx) else {
            return Ok(());
        };
        let (tx, msg_id, tx_hash) = build_inscription_transaction(channel, self.payload_bytes)?;
        submit_transaction_via_cluster(self.ctx, Arc::new(tx)).await?;

        channel.submitted += 1;
        channel.pending = Some(PendingSubmission {
            tx_hash,
            msg_id,
            submitted_at: Instant::now(),
        });
        self.pending_by_hash.insert(tx_hash, channel_idx);

        debug!(
            channel = ?channel.channel_id,
            parent = ?channel.parent,
            tx_hash = ?tx_hash,
            "submitted inscription transaction"
        );

        Ok(())
    }

    async fn wait_for_block_or_timeout(&mut self) -> Result<(), DynError> {
        let remaining = self.deadline.saturating_duration_since(TokioInstant::now());
        if remaining.is_zero() {
            return Ok(());
        }

        let wait_for = remaining.min(BLOCK_POLL_TIMEOUT);
        match timeout(wait_for, self.feed.recv()).await {
            Ok(Ok(block)) => self.process_block(block.as_ref()),
            Ok(Err(RecvError::Lagged(skipped))) => {
                warn!(skipped, "inscription workload block feed lagged");
            }
            Ok(Err(RecvError::Closed)) => {
                return Err(InscriptionWorkloadError::FeedClosed.into());
            }
            Err(_) => {}
        }

        Ok(())
    }

    fn process_block(&mut self, block: &BlockRecord) {
        for observed in &block.events {
            for tx in &observed.block.transactions {
                let tx_hash = tx.hash();
                let Some(channel_idx) = self.pending_by_hash.remove(&tx_hash) else {
                    continue;
                };

                let Some(channel) = self.channels.get_mut(channel_idx) else {
                    continue;
                };

                let Some(pending) = channel.pending.take() else {
                    continue;
                };

                channel.parent = pending.msg_id;
                channel.confirmed += 1;

                debug!(
                    channel = ?channel.channel_id,
                    tx_hash = ?pending.tx_hash,
                    confirmation_ms = pending.submitted_at.elapsed().as_millis(),
                    "inscription transaction confirmed"
                );
            }
        }
    }

    fn stats(&self) -> (u64, u64, u64) {
        let submitted = self.channels.iter().map(|ch| ch.submitted).sum();
        let confirmed = self.channels.iter().map(|ch| ch.confirmed).sum();
        let pending = self
            .channels
            .iter()
            .filter(|ch| ch.pending.is_some())
            .count() as u64;
        (submitted, confirmed, pending)
    }
}

fn build_channel_states(channel_count: usize, run_salt: &[u8; 32]) -> Vec<ChannelState> {
    let mut channels = Vec::with_capacity(channel_count);

    for index in 0..channel_count {
        let signing_key = derive_channel_signing_key(index, run_salt);
        let channel_id = channel_id_from_signing_key(&signing_key);

        channels.push(ChannelState {
            channel_id,
            signing_key,
            parent: MsgId::root(),
            next_nonce: 0,
            pending: None,
            submitted: 0,
            confirmed: 0,
        });
    }

    channels
}

fn derive_channel_signing_key(index: usize, run_salt: &[u8; 32]) -> Ed25519Key {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&(index as u64 + 1).to_le_bytes());
    for (position, byte) in run_salt.iter().copied().enumerate() {
        key[position] ^= byte;
    }
    key[31] = 0x7f;
    Ed25519Key::from_bytes(&key)
}

fn resolve_run_salt() -> [u8; 32] {
    // Keep channel identities unique between runs so external/devnet tests do
    // not keep reusing the same channel IDs.
    let run_id = format!(
        "auto-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0u128, |duration| duration.as_nanos())
    );

    let mut salt = [0u8; 32];
    for (position, byte) in run_id.as_bytes().iter().copied().enumerate() {
        salt[position % salt.len()] ^= byte;
    }

    salt
}

fn channel_id_from_signing_key(signing_key: &Ed25519Key) -> ChannelId {
    ChannelId::from(signing_key.public_key().to_bytes())
}

fn build_inscription_transaction(
    channel: &mut ChannelState,
    payload_bytes: usize,
) -> Result<(SignedMantleTx, MsgId, TxHash), DynError> {
    let op = InscriptionOp {
        channel_id: channel.channel_id,
        inscription: build_payload(channel, payload_bytes),
        parent: channel.parent,
        signer: channel.signing_key.public_key(),
    };
    let msg_id = op.id();

    let mantle_tx = MantleTx {
        ops: vec![Op::ChannelInscribe(op)],
        storage_gas_price: GENESIS_STORAGE_GAS_PRICE,
        execution_gas_price: 0.into(),
    };
    let tx_hash = mantle_tx.hash();

    let ed25519_signature = channel
        .signing_key
        .sign_payload(tx_hash.as_signing_bytes().as_ref());

    let signed_tx = SignedMantleTx::new(mantle_tx, vec![OpProof::Ed25519Sig(ed25519_signature)])
        .map_err(|error| InscriptionWorkloadError::SignedTransactionBuild(error.to_string()))?;

    channel.next_nonce = channel.next_nonce.saturating_add(1);

    Ok((signed_tx, msg_id, tx_hash))
}

fn build_payload(channel: &ChannelState, payload_bytes: usize) -> Vec<u8> {
    let mut payload = format!(
        "tf-inscription:{:?}:{}",
        channel.channel_id, channel.next_nonce
    )
    .into_bytes();

    if payload.len() < payload_bytes {
        payload.resize(payload_bytes, b'x');
    } else if payload.len() > payload_bytes {
        payload.truncate(payload_bytes);
    }

    payload
}

async fn submit_transaction_via_cluster(
    ctx: &RunContext<impl LbcScenarioEnv>,
    tx: Arc<SignedMantleTx>,
) -> Result<(), DynError> {
    let mut clients = ctx.node_clients().snapshot();
    if clients.is_empty() {
        return Err(cluster_client_exhausted_error());
    }

    clients.shuffle(&mut thread_rng());

    for attempt in 0..SUBMIT_RETRIES {
        match submit_to_clients(&mut clients, tx.as_ref(), attempt).await {
            Ok(()) => return Ok(()),
            Err(error) if attempt + 1 == SUBMIT_RETRIES => return Err(error),
            Err(_) => tokio::time::sleep(SUBMIT_RETRY_DELAY).await,
        }
    }

    Err(cluster_client_exhausted_error())
}

async fn submit_to_clients(
    clients: &mut [NodeHttpClient],
    tx: &SignedMantleTx,
    attempt: usize,
) -> Result<(), DynError> {
    let tx_hash = tx.hash();
    clients.shuffle(&mut thread_rng());

    let mut last_error = None;
    for client in clients {
        let url = client.base_url().clone();
        debug!(?tx_hash, %url, attempt, "submitting inscription transaction");

        match client.submit_transaction(tx).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error.into());
                debug!(?tx_hash, %url, attempt, "inscription transaction submission failed");
            }
        }
    }

    Err(last_error.unwrap_or_else(cluster_client_exhausted_error))
}

fn cluster_client_exhausted_error() -> DynError {
    InscriptionWorkloadError::ClusterClientExhausted.into()
}
