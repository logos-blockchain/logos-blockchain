//! Zone SDK test helpers shared by Cucumber steps.
//!
//! The helpers in this module keep the feature steps focused on scenario
//! intent: start a zone-backed node, run sequencers, publish messages, observe
//! the indexer, and submit the channel operations that the zone layer relies
//! on.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use lb_common_http_client::{CommonHttpClient, Slot};
use lb_core::{
    mantle::{
        MantleTx, Note, Op, OpProof, Transaction as _, Utxo, Value,
        ledger::{Inputs, Outputs, OutputsError},
        ops::{
            channel::{
                ChannelId,
                deposit::{DepositOp, Metadata},
                inscribe::Inscription,
                withdraw::ChannelWithdrawOp,
            },
            transfer::TransferOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
};
use lb_http_api_common::bodies::{
    channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
    wallet::sign::{WalletSignTxZkRequestBody, WalletSignTxZkResponseBody},
};
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey, ZkSignature};
use lb_node::SignedMantleTx;
use lb_testing_framework::NodeHttpClient;
use lb_zone_sdk::{
    ZoneMessage, adapter::NodeHttpClient as ZoneNodeHttpClient, indexer::ZoneIndexer,
    sequencer::ZoneSequencer,
};
use rand::{Rng as _, thread_rng};
use reqwest::Url;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::warn;

use super::runner::{
    self, ChannelUpdate, Event, FinalizedOp, InscriptionId, InscriptionInfo, OrphanedTx, PendingTx,
    PublishResult, SequencerChannelView, SequencerCheckpoint, SequencerClient, SequencerConfig,
    TurnNotification, TxStatus, TxStatusUpdate, WithdrawArg,
};
use crate::common::{
    chain::wait_for_transactions_inclusion, mantle_inscription::make_inscription,
    wallet::build_wallet_funded_transfer,
};

#[derive(Debug, thiserror::Error)]
pub enum ZoneTestError {
    #[error("timed out waiting for zone sequencer to accept a publish request")]
    PublishTimeout,
    #[error("zone indexer request failed: {message}")]
    Indexer { message: String },
    #[error("timed out waiting for zone indexer to return all messages")]
    IndexerTimeout,
    #[error("zone indexer returned {actual} copies of '{payload}', expected {expected}")]
    IndexedPayloadCountMismatch {
        payload: String,
        expected: usize,
        actual: usize,
    },
    #[error("timed out waiting for zone transactions to appear on the canonical chain")]
    InclusionTimeout,
    #[error("failed to fetch consensus info while checking finalized transactions: {message}")]
    Consensus { message: String },
    #[error("failed to fetch block while checking finalized transactions: {message}")]
    Block { message: String },
    #[error("timed out waiting for zone transactions to finalize")]
    FinalizationTimeout,
    #[error("timed out waiting for zone LIB to advance")]
    LibAdvanceTimeout,
    #[error("timed out waiting for zone sequencer channel view condition: {message}")]
    ChannelViewTimeout { message: String },
    #[error("failed to find a funding note with exact value {value}")]
    MissingExactFundingNote { value: Value },
    #[error("failed to submit zone deposit: {message}")]
    SubmitDeposit { message: String },
    #[error("failed to sign zone transaction: {message}")]
    SignTransaction { message: String },
    #[error("failed to build atomic zone deposit transaction: {message}")]
    BuildAtomicDeposit { message: String },
    #[error("failed to submit atomic zone deposit transaction: {message}")]
    SubmitAtomicDeposit { message: String },
    #[error("failed to submit zone withdraw transaction: {message}")]
    SubmitWithdraw { message: String },
    #[error("timed out waiting for zone withdraw to appear in the indexer")]
    WithdrawTimeout,
    #[error("zone sequencer event stream stopped before observing the expected event")]
    SequencerStopped,
    #[error(transparent)]
    BoundedError(#[from] lb_utils::bounded_vec::BoundedError),
    #[error(transparent)]
    OutputsError(#[from] OutputsError),
}

/// Result of an atomic deposit scenario where a deposit and zone inscription
/// are submitted as one Mantle transaction.
pub struct AtomicZoneDepositSubmission {
    pub deposit: DepositOp,
    pub publish: PublishResult,
    pub reserved_inputs: Vec<Utxo>,
}

pub struct AtomicZoneDepositRequest {
    pub channel_id: ChannelId,
    pub funding_public_key: ZkPublicKey,
    pub available_utxos: Vec<Utxo>,
    pub amount: Value,
    pub inscription_data: Inscription,
    pub metadata: Metadata,
}

/// Result of a withdraw scenario where the zone sequencer signs the channel
/// withdraw and publishes the accompanying inscription.
pub struct ZoneWithdrawSubmission {
    pub withdraw: ChannelWithdrawOp,
    pub publish: PublishResult,
}

pub struct ZoneDeposit {
    pub deposit: DepositOp,
    pub reserved_inputs: Vec<Utxo>,
}

pub type DiscardedPayloads = Arc<tokio::sync::Mutex<HashSet<Inscription>>>;
pub type ZoneAccountBalances = HashMap<String, i64>;

/// Shared deadline for a publish attempt and the matching event wait so the
/// whole operation has one timeout budget.
#[derive(Clone, Copy)]
pub struct PublishDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl PublishDeadline {
    #[must_use]
    pub fn from_now(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    fn is_expired(self) -> bool {
        self.started_at.elapsed() > self.timeout
    }
}

/// Bundle returned from policy starters so callers can wire the cucumber
/// world. Wraps [`runner::Runtime`] — events and checkpoints are exposed
/// uniformly across all policies because the policy runs inline on the
/// drive task; the event mpsc is purely for test observation.
pub struct PolicyRuntime {
    pub task: JoinHandle<()>,
    pub client: SequencerClient<ZoneNodeHttpClient>,
    pub events: tokio::sync::broadcast::Receiver<Event>,
    pub checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    pub ready_rx: tokio::sync::watch::Receiver<bool>,
    pub channel_view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
    pub turn_to_write_rx: tokio::sync::watch::Receiver<TurnNotification>,
    pub tx_status_rx: tokio::sync::broadcast::Receiver<TxStatusUpdate>,
}

fn to_policy_runtime(rt: runner::Runtime<ZoneNodeHttpClient>) -> PolicyRuntime {
    PolicyRuntime {
        task: rt.task,
        client: rt.client,
        events: rt.event_rx,
        checkpoint_rx: rt.checkpoint_rx,
        ready_rx: rt.ready_rx,
        channel_view_rx: rt.channel_view_rx,
        turn_to_write_rx: rt.turn_to_write_rx,
        tx_status_rx: rt.tx_status_rx,
    }
}

/// Spawn a sequencer drive task with a no-op policy. Step bodies drive
/// publishes via [`SequencerClient`]; events flow to `PolicyRuntime.events`.
/// If `republish_orphans` is set, the [`OrphanRepublishPolicy`] runs inline
/// inside the drive loop.
pub fn start_sequencer_event_loop(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    republish_orphans: bool,
) -> PolicyRuntime {
    if republish_orphans {
        to_policy_runtime(runner::spawn(sequencer, OrphanRepublishPolicy))
    } else {
        to_policy_runtime(runner::spawn(sequencer, runner::PassivePolicy))
    }
}

/// Drives a competing-sequencer policy that re-publishes invalidated payloads
/// until they are either pending again or adopted on chain.
pub fn start_republish_policy(sequencer: ZoneSequencer<ZoneNodeHttpClient>) -> PolicyRuntime {
    to_policy_runtime(runner::spawn(sequencer, OrphanRepublishPolicy))
}

/// Drives a policy that republishes orphaned balance updates only when the
/// local canonical view can still apply the update without going negative,
/// and lays planned balance updates whenever it's our turn to write.
pub fn start_balance_aware_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    initial_balances: ZoneAccountBalances,
    planned_payloads: Vec<Inscription>,
) -> PolicyRuntime {
    let view_rx = sequencer.subscribe_channel_view();
    let policy = BalanceAwarePolicy {
        balances: BalanceAwareState::new(initial_balances),
        planned: VecDeque::from(planned_payloads),
        view_rx,
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// Drives a deterministic conflict policy used by tests that expect the final
/// zone chain to converge to sorted payload order.
pub fn start_sorted_conflict_policy(
    sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    discarded: &DiscardedPayloads,
) -> PolicyRuntime {
    let policy = SortedConflictPolicy {
        state: SortedConflictState::new(Arc::clone(discarded)),
    };
    to_policy_runtime(runner::spawn(sequencer, policy))
}

/// Inline policy: republish any orphaned inscriptions. Plain inscriptions
/// only — bundles are not auto-republished (callers that issue bundles
/// re-prepare with fresh withdraw nonces themselves).
struct OrphanRepublishPolicy;

impl<Node> runner::Policy<Node> for OrphanRepublishPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed { channel_update, .. } = event else {
            return;
        };
        for entry in &channel_update.orphaned {
            let OrphanedTx::Inscription(info) = entry else {
                continue;
            };
            if let Err(error) = sequencer.handle().publish(info.payload.clone()) {
                warn!(%error, "Failed to re-publish orphaned zone payload");
            }
        }
    }
}

/// Inline policy: republish orphans only when the local balance view still
/// allows it; publish planned payloads as soon as it's our turn to write.
struct BalanceAwarePolicy {
    balances: BalanceAwareState,
    planned: VecDeque<Inscription>,
    view_rx: tokio::sync::watch::Receiver<SequencerChannelView>,
}

impl<Node> runner::Policy<Node> for BalanceAwarePolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        if let Event::BlocksProcessed { channel_update, .. } = event {
            let ChannelUpdate { orphaned, adopted } = channel_update;
            let orphaned_inscriptions: Vec<InscriptionInfo> = orphaned
                .iter()
                .filter_map(|o| match o {
                    OrphanedTx::Inscription(i) => Some(i.clone()),
                    OrphanedTx::AtomicWithdraw(_) => None,
                })
                .collect();
            self.balances
                .remove_orphaned_payloads(&orphaned_inscriptions);
            self.balances.record_adopted_payloads(adopted);
            for info in orphaned_inscriptions {
                if !self.balances.should_republish(&info.payload) {
                    continue;
                }
                if let Err(error) = sequencer.handle().publish(info.payload.clone()) {
                    warn!(%error, "Failed to re-publish balance-aware zone payload");
                    continue;
                }
                self.balances.record_republished_payload(&info.payload);
            }
        }

        if !self.view_rx.borrow().our_turn_to_write {
            return;
        }
        while let Some(payload) = self.planned.pop_front() {
            if !self.balances.should_republish(&payload) {
                continue;
            }
            if let Err(error) = sequencer.handle().publish(payload.clone()) {
                warn!(%error, "Failed to publish planned balance-aware zone payload");
                self.planned.push_front(payload);
                break;
            }
            self.balances.record_republished_payload(&payload);
        }
    }
}

/// Inline policy: republish orphans only when they preserve sorted-payload
/// order; otherwise mark them as discarded.
struct SortedConflictPolicy {
    state: SortedConflictState,
}

impl<Node> runner::Policy<Node> for SortedConflictPolicy
where
    Node: lb_zone_sdk::adapter::Node + Clone + Send + Sync + 'static,
{
    async fn on_event(&mut self, sequencer: &mut ZoneSequencer<Node>, event: &Event) {
        let Event::BlocksProcessed { channel_update, .. } = event else {
            return;
        };
        let ChannelUpdate { orphaned, adopted } = channel_update;
        self.state.record_adoptions(adopted).await;
        for entry in orphaned {
            let OrphanedTx::Inscription(inscription) = entry else {
                continue;
            };
            if self.state.already_discarded(&inscription.payload).await {
                continue;
            }
            if self.state.preserves_order(inscription) {
                if let Err(error) = sequencer.handle().publish(inscription.payload.clone()) {
                    warn!(%error, "Failed to re-publish sorted zone payload");
                    continue;
                }
                self.state
                    .record_published_payload(inscription.payload.clone())
                    .await;
            } else {
                self.state.discard(inscription.payload.clone()).await;
            }
        }
    }
}

struct BalanceAwareState {
    initial_balances: ZoneAccountBalances,
    applied: HashMap<String, HashMap<String, i64>>,
}

impl BalanceAwareState {
    fn new(initial_balances: ZoneAccountBalances) -> Self {
        Self {
            initial_balances,
            applied: HashMap::new(),
        }
    }

    fn record_applied_payload(&mut self, payload: &Inscription) {
        let Some((uuid, account, delta)) = parse_balance_payload(payload) else {
            return;
        };

        self.applied.entry(account).or_default().insert(uuid, delta);
    }

    fn remove_orphaned_payloads(&mut self, orphaned: &[InscriptionInfo]) {
        for inscription in orphaned {
            let Some((uuid, account, _)) = parse_balance_payload(&inscription.payload) else {
                continue;
            };

            if let Some(account_updates) = self.applied.get_mut(&account) {
                account_updates.remove(&uuid);
            }
        }
    }

    fn record_adopted_payloads(&mut self, adopted: &[InscriptionInfo]) {
        for inscription in adopted {
            self.record_applied_payload(&inscription.payload);
        }
    }

    fn should_republish(&self, payload: &Inscription) -> bool {
        let Some((uuid, account, delta)) = parse_balance_payload(payload) else {
            return false;
        };

        if self.account_updates(&account).contains_key(&uuid) {
            return false;
        }

        self.available_balance(&account) + delta >= 0
    }

    fn record_republished_payload(&mut self, payload: &Inscription) {
        self.record_applied_payload(payload);
    }

    fn available_balance(&self, account: &str) -> i64 {
        self.initial_balances.get(account).copied().unwrap_or(0)
            + self.account_updates(account).values().sum::<i64>()
    }

    fn account_updates(&self, account: &str) -> &HashMap<String, i64> {
        self.applied.get(account).unwrap_or(&EMPTY_BALANCE_UPDATES)
    }
}

static EMPTY_BALANCE_UPDATES: LazyLock<HashMap<String, i64>> = LazyLock::new(HashMap::new);

struct SortedConflictState {
    max_seen_on_chain: Option<Inscription>,
    discarded: DiscardedPayloads,
}

impl SortedConflictState {
    const fn new(discarded: DiscardedPayloads) -> Self {
        Self {
            max_seen_on_chain: None,
            discarded,
        }
    }

    async fn record_adoptions(&mut self, adopted: &[InscriptionInfo]) {
        for payload in adopted {
            self.discarded.lock().await.remove(&payload.payload);
            self.record_seen_payload(payload.payload.clone());
        }
    }

    async fn record_published_payload(&mut self, payload: Inscription) {
        self.discarded.lock().await.remove(&payload);
        self.record_seen_payload(payload);
    }

    fn record_seen_payload(&mut self, payload: Inscription) {
        if self
            .max_seen_on_chain
            .as_ref()
            .is_none_or(|seen| payload > *seen)
        {
            self.max_seen_on_chain = Some(payload);
        }
    }

    async fn already_discarded(&self, payload: &Inscription) -> bool {
        self.discarded.lock().await.contains(payload)
    }

    fn preserves_order(&self, inscription: &InscriptionInfo) -> bool {
        self.max_seen_on_chain
            .as_deref()
            .is_none_or(|seen| inscription.payload.as_slice() >= seen)
    }

    async fn discard(&self, payload: Inscription) {
        self.discarded.lock().await.insert(payload);
    }
}

/// Creates a scenario-local sequencer key.
#[must_use]
pub fn keygen() -> Ed25519Key {
    let mut key_bytes = [0u8; 32];
    thread_rng().fill(&mut key_bytes);
    Ed25519Key::from_bytes(&key_bytes)
}

/// Encodes a balance-affecting zone payload used by balance-aware sequencer
/// scenarios.
#[must_use]
pub fn balance_update_payload(uuid: &str, account: &str, delta: i64) -> Inscription {
    make_inscription(&format!("{uuid}:{account}:{delta}"))
}

/// Parses a balance-affecting payload in the same format produced by
/// [`balance_update_payload`].
pub fn parse_balance_payload(payload: &Inscription) -> Option<(String, String, i64)> {
    let payload = std::str::from_utf8(payload.as_slice()).ok()?;
    let parts = payload.splitn(3, ':').collect::<Vec<_>>();
    let [uuid, account, delta] = parts.as_slice() else {
        return None;
    };

    Some((
        (*uuid).to_owned(),
        (*account).to_owned(),
        delta.parse().ok()?,
    ))
}

/// Uses a short resubmit interval so retry-sensitive zone scenarios settle
/// quickly enough for CI.
#[must_use]
pub fn sequencer_config() -> SequencerConfig {
    SequencerConfig {
        resubmit_interval: Duration::from_secs(3),
        min_slots_remaining_in_turn: 2,
        ..SequencerConfig::default()
    }
}

/// Uses the same retry profile while overriding pending publish submit depth.
#[must_use]
pub fn sequencer_config_with_pending_submit_depth(
    max_pending_publish_depth: usize,
) -> SequencerConfig {
    SequencerConfig {
        max_pending_publish_depth,
        ..sequencer_config()
    }
}

/// Publishes a zone payload synchronously through the runner and returns the
/// SDK's [`PublishResult`] inline. Retries transient publish errors until
/// the deadline elapses. No "wait for event" — the new SDK publishes
/// synchronously and the runner forwards the call through the drive task.
pub async fn publish_message_with_retry(
    client: &SequencerClient<ZoneNodeHttpClient>,
    data: &Inscription,
    deadline: PublishDeadline,
) -> Result<PublishResult, ZoneTestError> {
    loop {
        if deadline.is_expired() {
            return Err(ZoneTestError::PublishTimeout);
        }
        match client.publish(data.clone()).await {
            Ok((result, _cp)) => return Ok(result),
            Err(error) => {
                warn!(error = %error, "Zone sequencer publish failed, retrying");
                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Waits until the sequencer's event stream surfaces the payload in
/// [`ChannelUpdate::adopted`] while collecting any mempool-pending events
/// passed on the same event stream — i.e. the inscription was published and
/// landed on the canonical chain. This is the end-to-end signal a real
/// SDK consumer would observe.
pub async fn wait_for_adopted_payload_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    data: &[u8],
    duration: Duration,
) -> Result<(PublishResult, HashSet<InscriptionId>), ZoneTestError> {
    let mut mempool_pending = HashSet::new();
    timeout(duration, async {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("event subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            if let Event::MempoolPending(tx_hash) = event {
                mempool_pending.insert(tx_hash);
                continue;
            }
            let Event::BlocksProcessed { channel_update, .. } = event else {
                continue;
            };
            for info in channel_update.adopted {
                if info.payload.as_slice() == data {
                    return Ok((
                        PublishResult {
                            tx: PendingTx::Inscription(info),
                        },
                        mempool_pending,
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
}

/// Waits for every payload in `data` to appear in
/// [`ChannelUpdate::adopted`], while collecting any mempool-pending events
/// passed on the same event stream.
pub async fn wait_for_adopted_payloads_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    data: &[Inscription],
    duration: Duration,
) -> Result<(Vec<PublishResult>, HashSet<InscriptionId>), ZoneTestError> {
    timeout(duration, async {
        let mut results: Vec<Option<PublishResult>> =
            std::iter::repeat_with(|| None).take(data.len()).collect();
        let mut remaining = data.len();
        let mut mempool_pending = HashSet::new();

        while remaining > 0 {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("event subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            if let Event::MempoolPending(tx_hash) = event {
                mempool_pending.insert(tx_hash);
                continue;
            }
            let Event::BlocksProcessed { channel_update, .. } = event else {
                continue;
            };
            for info in channel_update.adopted {
                let payload = info.payload.as_slice();
                let Some(index) = data.iter().enumerate().find_map(|(index, expected)| {
                    (results[index].is_none() && payload == expected.as_slice()).then_some(index)
                }) else {
                    continue;
                };
                results[index] = Some(PublishResult {
                    tx: PendingTx::Inscription(info),
                });
                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }

        Ok((results.into_iter().flatten().collect(), mempool_pending))
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
}

pub async fn wait_for_tx_status_lifecycle(
    tx_status_rx: &mut tokio::sync::broadcast::Receiver<TxStatusUpdate>,
    tx_hashes: &[InscriptionId],
    statuses: &[TxStatus],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let mut remaining: HashSet<(InscriptionId, TxStatus)> = tx_hashes
        .iter()
        .flat_map(|tx_hash| statuses.iter().map(move |status| (*tx_hash, *status)))
        .collect();

    timeout(duration, async {
        while !remaining.is_empty() {
            let update = match tx_status_rx.recv().await {
                Ok(update) => update,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("tx-status subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            remaining.remove(&(update.tx_hash, update.status));
            if remaining.is_empty() {
                return Ok(());
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

/// Waits until the subscribed channel view satisfies the supplied predicate.
pub async fn wait_for_channel_view(
    view_rx: &mut tokio::sync::watch::Receiver<SequencerChannelView>,
    duration: Duration,
    predicate: impl Fn(&SequencerChannelView) -> bool + Send + Sync,
) -> Result<SequencerChannelView, ZoneTestError> {
    timeout(duration, async {
        loop {
            let current = view_rx.borrow().clone();
            if predicate(&current) {
                return Ok(current);
            }

            view_rx
                .changed()
                .await
                .map_err(|error| ZoneTestError::Indexer {
                    message: format!("channel view sender closed: {error}"),
                })?;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelViewTimeout {
        message: format!(
            "condition not reached within {} seconds",
            duration.as_secs()
        ),
    })?
}

/// Waits until the sequencer emits a turn-to-write notification.
pub async fn wait_for_turn_to_write(
    turn_rx: &mut tokio::sync::watch::Receiver<TurnNotification>,
    duration: Duration,
) -> Result<TurnNotification, ZoneTestError> {
    timeout(duration, async {
        loop {
            let current = turn_rx.borrow().clone();
            if current.our_turn_to_write {
                return Ok(current);
            }

            turn_rx
                .changed()
                .await
                .map_err(|error| ZoneTestError::Indexer {
                    message: format!("turn-to-write sender closed: {error}"),
                })?;
        }
    })
    .await
    .map_err(|_| ZoneTestError::ChannelViewTimeout {
        message: format!(
            "turn to write not reached within {} seconds",
            duration.as_secs()
        ),
    })?
}

/// Collects indexed block payloads until all expected messages have appeared.
///
/// The returned order is the order observed from the indexer, which lets
/// assertions decide whether ordering matters for the scenario.
pub async fn collect_indexed_messages(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_messages: &[Inscription],
    duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let expected: HashSet<Inscription> = expected_messages.iter().cloned().collect();
    let mut seen: HashSet<Inscription> = HashSet::new();
    let mut ordered: Vec<Inscription> = Vec::new();

    poll_zone_indexer_until(
        indexer,
        duration,
        || ZoneTestError::IndexerTimeout,
        |message| {
            let ZoneMessage::Block(block) = message else {
                return None;
            };

            if expected.contains(&block.data) && seen.insert(block.data.clone()) {
                ordered.push(block.data.clone());
            }

            (seen == expected).then(|| ordered.clone())
        },
    )
    .await
}

/// Replays the indexer stream until it exactly matches the expected message
/// sequence without duplicates.
pub async fn collect_indexed_messages_exactly_once(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_messages: &[Inscription],
    duration: Duration,
) -> Result<Vec<Inscription>, ZoneTestError> {
    let expected: HashSet<Inscription> = expected_messages.iter().cloned().collect();

    timeout(duration, async {
        loop {
            let mut ordered = Vec::new();
            let mut cursor = None;

            loop {
                let stream = indexer.next_messages(cursor).await.map_err(|error| {
                    ZoneTestError::Indexer {
                        message: error.to_string(),
                    }
                })?;
                futures::pin_mut!(stream);

                let mut saw_message = false;

                while let Some((message, slot)) = stream.next().await {
                    saw_message = true;
                    cursor = Some(slot);

                    if let ZoneMessage::Block(block) = message
                        && expected.contains(&block.data)
                    {
                        ordered.push(block.data);
                    }
                }

                if !saw_message {
                    break;
                }
            }

            if ordered == expected_messages {
                return Ok(ordered);
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

/// Waits until the indexer returns exactly `expected_count` copies of one
/// payload after a short settle period.
///
/// This intentionally counts duplicate payload bytes, which is required for
/// shared-payload zone tests where each inscription has the same data but a
/// distinct transaction lineage.
pub async fn wait_for_exact_indexed_payload_count(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_payload: Inscription,
    expected_count: usize,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let count = count_indexed_payload(indexer, expected_payload.clone()).await?;

            if count >= expected_count {
                sleep(Duration::from_secs(30)).await;

                let final_count = count_indexed_payload(indexer, expected_payload.clone()).await?;
                if final_count == expected_count {
                    return Ok(());
                }

                return Err(ZoneTestError::IndexedPayloadCountMismatch {
                    payload: String::from_utf8_lossy(expected_payload.as_slice()).to_string(),
                    expected: expected_count,
                    actual: final_count,
                });
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::IndexerTimeout)?
}

async fn count_indexed_payload(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_payload: Inscription,
) -> Result<usize, ZoneTestError> {
    let mut count = 0;
    let mut cursor = None;

    loop {
        let stream =
            indexer
                .next_messages(cursor)
                .await
                .map_err(|error| ZoneTestError::Indexer {
                    message: error.to_string(),
                })?;
        futures::pin_mut!(stream);

        let mut saw_message = false;

        while let Some((message, slot)) = stream.next().await {
            saw_message = true;
            cursor = Some(slot);
            if let ZoneMessage::Block(block) = message
                && block.data == expected_payload
            {
                count += 1;
            }
        }

        if !saw_message {
            return Ok(count);
        }
    }
}

/// Waits until the zone indexer observes the expected channel deposit,
/// including its amount.
pub async fn wait_for_deposit(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &DepositOp,
    expected_amount: Value,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_zone_indexer_until(
        indexer,
        duration,
        || ZoneTestError::IndexerTimeout,
        |message| match message {
            ZoneMessage::Deposit(deposit)
                if deposit.inputs == expected.inputs
                    && deposit.amount == expected_amount
                    && deposit.metadata() == expected.metadata.as_slice() =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await
}

async fn poll_zone_indexer_until<T>(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    duration: Duration,
    timeout_error: impl FnOnce() -> ZoneTestError,
    mut predicate: impl FnMut(&ZoneMessage) -> Option<T>,
) -> Result<T, ZoneTestError> {
    timeout(duration, async {
        let mut cursor = None;

        loop {
            let stream =
                indexer
                    .next_messages(cursor)
                    .await
                    .map_err(|error| ZoneTestError::Indexer {
                        message: error.to_string(),
                    })?;
            futures::pin_mut!(stream);

            while let Some((message, slot)) = stream.next().await {
                cursor = Some(slot);

                if let Some(result) = predicate(&message) {
                    return Ok(result);
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| timeout_error())?
}

/// Waits until the zone indexer observes the expected channel withdraw.
pub async fn wait_for_withdraw(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &ChannelWithdrawOp,
    timeout_duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_zone_indexer_until(
        indexer,
        timeout_duration,
        || ZoneTestError::WithdrawTimeout,
        |message| match message {
            ZoneMessage::Withdraw(withdraw) if withdraw.outputs == expected.outputs => Some(()),
            _ => None,
        },
    )
    .await
}

/// Waits until the sequencer's event stream surfaces the expected deposit
/// in [`Event::BlocksProcessed::finalized`] (matched by `inputs`, `amount`,
/// and `metadata`) while collecting any mempool-pending events. Drains the
/// events channel as it goes — call this after any earlier event consumers in
/// the scenario have moved past the relevant publish events.
pub async fn wait_for_finalized_deposit_via_sequencer_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    expected: &DepositOp,
    expected_amount: Value,
    duration: Duration,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    poll_sequencer_finalized_until_and_collect_mempool_pending(
        events,
        duration,
        ZoneTestError::IndexerTimeout,
        |op| {
            matches!(op, FinalizedOp::Deposit(d)
            if d.inputs == expected.inputs
                && d.amount == expected_amount
                && d.metadata == expected.metadata)
        },
    )
    .await
}

/// Waits until the sequencer's event stream surfaces the expected withdraw
/// (matched by `outputs`) while collecting any mempool-pending events. Drains
/// the events channel as it goes.
pub async fn wait_for_finalized_withdraw_via_sequencer_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    expected: &ChannelWithdrawOp,
    duration: Duration,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    poll_sequencer_finalized_until_and_collect_mempool_pending(
        events,
        duration,
        ZoneTestError::WithdrawTimeout,
        |op| matches!(op, FinalizedOp::Withdraw(w) if w.op.outputs == expected.outputs),
    )
    .await
}

async fn poll_sequencer_finalized_until_and_collect_mempool_pending(
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    duration: Duration,
    timeout_error: ZoneTestError,
    mut predicate: impl FnMut(&FinalizedOp) -> bool,
) -> Result<HashSet<InscriptionId>, ZoneTestError> {
    timeout(duration, async {
        let mut mempool_pending = HashSet::new();
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("event subscriber lagged by {n}, recovering");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(ZoneTestError::SequencerStopped);
                }
            };
            if let Event::MempoolPending(tx_hash) = event {
                mempool_pending.insert(tx_hash);
                continue;
            }
            let Event::BlocksProcessed { finalized, .. } = event else {
                continue;
            };
            for tx in finalized {
                if tx.ops.iter().any(&mut predicate) {
                    return Ok(mempool_pending);
                }
            }
        }
    })
    .await
    .map_err(|_| timeout_error)?
}

/// Waits until node mempool/chain observation confirms the submitted zone
/// transactions reached the canonical chain.
pub async fn ensure_zone_transactions_included(
    client: &NodeHttpClient,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let included = wait_for_transactions_inclusion(client, tx_hashes, duration).await;

    if included {
        return Ok(());
    }

    Err(ZoneTestError::InclusionTimeout)
}

/// Walks back from LIB until every expected zone transaction is found in the
/// finalized chain.
pub async fn wait_for_transactions_finalized(
    node_url: Url,
    tx_hashes: &[InscriptionId],
    duration: Duration,
) -> Result<(), ZoneTestError> {
    let client = CommonHttpClient::new(None);
    let expected: HashSet<_> = tx_hashes.iter().copied().collect();

    timeout(duration, async {
        loop {
            let info = client
                .consensus_info(node_url.clone())
                .await
                .map_err(|error| ZoneTestError::Consensus {
                    message: error.to_string(),
                })?;

            let mut found = HashSet::new();
            let mut current = info.cryptarchia_info.lib;

            while let Some(block) = client
                .get_block_by_id(node_url.clone(), current)
                .await
                .map_err(|error| ZoneTestError::Block {
                    message: error.to_string(),
                })?
            {
                for tx in &block.transactions {
                    let hash = tx.mantle_tx.hash();
                    if expected.contains(&hash) {
                        found.insert(hash);
                    }
                }

                current = block.header.parent_block;
            }

            if found == expected {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::FinalizationTimeout)?
}

/// Waits for LIB movement after a restart so stale-checkpoint scenarios can
/// distinguish old local state from new canonical chain progress.
pub async fn wait_for_lib_advance(
    client: &NodeHttpClient,
    initial_lib_slot: Slot,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let info = client
                .consensus_info()
                .await
                .map_err(|error| ZoneTestError::Consensus {
                    message: error.to_string(),
                })?;

            if info.cryptarchia_info.lib_slot > initial_lib_slot {
                return Ok(());
            }

            sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| ZoneTestError::LibAdvanceTimeout)?
}

/// Builds a regular channel deposit for an existing funding note with the
/// exact deposit value.
pub fn build_zone_deposit(
    available_utxos: Vec<Utxo>,
    channel_id: ChannelId,
    amount: Value,
    metadata: Metadata,
) -> Result<ZoneDeposit, ZoneTestError> {
    let note = available_utxos
        .into_iter()
        .find(|utxo| utxo.note.value == amount)
        .ok_or(ZoneTestError::MissingExactFundingNote { value: amount })?;

    Ok(ZoneDeposit {
        deposit: DepositOp {
            channel_id,
            inputs: Inputs::new([note.id()]),
            metadata,
        },
        reserved_inputs: vec![note],
    })
}

/// Submits a regular channel deposit through the node wallet API.
pub async fn submit_zone_deposit(
    node_url: &Url,
    deposit: &DepositOp,
    funding_public_key: ZkPublicKey,
) -> Result<InscriptionId, ZoneTestError> {
    let body = ChannelDepositRequestBody {
        tip: None,
        deposit: deposit.clone(),
        change_public_key: funding_public_key,
        funding_public_keys: vec![funding_public_key],
        max_tx_fee: 10.into(),
    };

    let request_url =
        node_url
            .join("/channel/deposit")
            .map_err(|error| ZoneTestError::SubmitDeposit {
                message: error.to_string(),
            })?;

    let response: ChannelDepositResponseBody = CommonHttpClient::new(None)
        .post(request_url, &body)
        .await
        .map_err(|error| ZoneTestError::SubmitDeposit {
            message: error.to_string(),
        })?;

    Ok(response.hash)
}

/// Builds and submits a single transaction that both creates the deposit note
/// and publishes the zone inscription that consumes it.
pub async fn submit_atomic_zone_deposit(
    node_url: &Url,
    client: &SequencerClient<ZoneNodeHttpClient>,
    request: AtomicZoneDepositRequest,
) -> Result<AtomicZoneDepositSubmission, ZoneTestError> {
    let AtomicZoneDepositRequest {
        channel_id,
        funding_public_key,
        available_utxos,
        amount,
        metadata,
        inscription_data,
    } = request;
    let (transfer, reserved_inputs) =
        build_atomic_deposit_transfer(available_utxos, funding_public_key, amount)?;
    let deposit = build_atomic_deposit_op(channel_id, metadata, &transfer)?;

    let (tx, msg_id, sequencer_sig) = client
        .prepare_tx(
            [Op::Transfer(transfer), Op::ChannelDeposit(deposit.clone())].into(),
            inscription_data,
        )
        .await
        .map_err(|error| ZoneTestError::BuildAtomicDeposit {
            message: error.to_string(),
        })?;

    let user_sig = sign_tx_zk(node_url, &tx, vec![funding_public_key]).await?;
    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ZkSig(user_sig.clone()),
            OpProof::ZkSig(user_sig),
            OpProof::Ed25519Sig(sequencer_sig),
        ],
    )
    .map_err(|error| ZoneTestError::SubmitAtomicDeposit {
        message: error.to_string(),
    })?;

    let (result, _cp) = client
        .submit_signed_tx(signed_tx, msg_id)
        .await
        .map_err(|error| ZoneTestError::SubmitAtomicDeposit {
            message: error.to_string(),
        })?;

    Ok(AtomicZoneDepositSubmission {
        deposit,
        publish: result,
        reserved_inputs,
    })
}

/// Builds the funding transfer that creates the note consumed by an atomic
/// zone deposit.
fn build_atomic_deposit_transfer(
    available_utxos: Vec<Utxo>,
    funding_public_key: ZkPublicKey,
    amount: Value,
) -> Result<(TransferOp, Vec<Utxo>), ZoneTestError> {
    let deposit_note = Note::new(amount, funding_public_key);
    let funded_transfer =
        build_wallet_funded_transfer(available_utxos, vec![deposit_note], funding_public_key)
            .map_err(|error| ZoneTestError::BuildAtomicDeposit {
                message: error.to_string(),
            })?;

    Ok(funded_transfer.into_parts())
}

/// Points the channel deposit at the note created by the atomic funding
/// transfer, keeping both operations in the same transaction.
fn build_atomic_deposit_op(
    channel_id: ChannelId,
    metadata: Metadata,
    transfer: &TransferOp,
) -> Result<DepositOp, ZoneTestError> {
    let deposit_note_id = transfer
        .outputs
        .utxo_by_index(0, transfer)
        .ok_or_else(|| ZoneTestError::BuildAtomicDeposit {
            message: "transfer did not produce the deposit note".to_owned(),
        })?
        .id();

    Ok(DepositOp {
        channel_id,
        inputs: Inputs::new([deposit_note_id]),
        metadata,
    })
}

/// Submits a channel withdraw signed by the active zone sequencer and publishes
/// the withdraw inscription as part of the same SDK flow.
pub async fn submit_zone_withdraw(
    client: &SequencerClient<ZoneNodeHttpClient>,
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    inscription_data: Inscription,
) -> Result<ZoneWithdrawSubmission, ZoneTestError> {
    let withdraw = ChannelWithdrawOp {
        channel_id,
        outputs: Outputs::new([Note::new(amount, funding_public_key)]),
        withdraw_nonce: 0,
    };

    let (tx, msg_id, inscription_sig) = client
        .prepare_tx(
            [Op::ChannelWithdraw(withdraw.clone())].into(),
            inscription_data,
        )
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    let withdraw_sig =
        client
            .sign_tx(&tx)
            .await
            .map_err(|error| ZoneTestError::SubmitWithdraw {
                message: error.to_string(),
            })?;

    let withdraw_proof =
        match ChannelMultiSigProof::new(vec![IndexedSignature::new(0, withdraw_sig)]) {
            Ok(proof) => proof,
            Err(error) => {
                return Err(ZoneTestError::SubmitWithdraw {
                    message: error.to_string(),
                });
            }
        };

    let signed_tx = SignedMantleTx::new(
        tx,
        vec![
            OpProof::ChannelMultiSigProof(withdraw_proof),
            OpProof::Ed25519Sig(inscription_sig),
        ],
    )
    .map_err(|error| ZoneTestError::SubmitWithdraw {
        message: error.to_string(),
    })?;

    let (result, _cp) = client
        .submit_signed_tx(signed_tx, msg_id)
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    Ok(ZoneWithdrawSubmission {
        withdraw,
        publish: result,
    })
}

/// Result of publishing an atomic inscription+withdraw bundle. Carries every
/// withdraw op produced by the SDK (one per `WithdrawArg`, in submission
/// order) so a multi-withdraw scenario can match each by its outputs.
pub struct ZoneAtomicWithdrawSubmission {
    pub withdraws: Vec<ChannelWithdrawOp>,
    pub publish: PublishResult,
}

/// Publishes an atomic inscription+withdraw bundle through the runner.
/// Returns every withdraw op (with the nonce filled by the SDK) from the
/// publish call's return value, so downstream cucumber assertions can
/// match each withdraw by its outputs.
///
/// `outputs_per_arg` carries one entry per `WithdrawArg`; each inner `Vec`
/// becomes that arg's `Outputs` (one `Note::new(amount, funding_pk)` per
/// listed amount). Exercises the SDK API at full width: multiple args, with
/// any arg able to carry multiple output notes.
pub async fn publish_atomic_zone_withdraw(
    client: &SequencerClient<ZoneNodeHttpClient>,
    funding_public_key: ZkPublicKey,
    outputs_per_arg: Vec<Vec<Value>>,
    inscription_data: Inscription,
    _deadline: PublishDeadline,
) -> Result<ZoneAtomicWithdrawSubmission, ZoneTestError> {
    if outputs_per_arg.is_empty() {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_zone_withdraw requires at least one withdraw arg".to_owned(),
        });
    }
    let withdraw_args: Vec<WithdrawArg> = outputs_per_arg
        .iter()
        .map(|amounts| {
            Ok::<WithdrawArg, ZoneTestError>(WithdrawArg {
                outputs: Outputs::try_new(
                    amounts
                        .iter()
                        .map(|amount| Note::new(*amount, funding_public_key))
                        .collect::<Vec<_>>(),
                )?,
            })
        })
        .collect::<Result<Vec<_>, ZoneTestError>>()?;

    let (result, _cp) = client
        .publish_atomic_withdraw(inscription_data, withdraw_args)
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    let PendingTx::AtomicWithdraw(info) = result.tx else {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_withdraw returned a non-AtomicWithdraw publish result"
                .to_owned(),
        });
    };
    if info.withdraws.is_empty() {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "atomic withdraw bundle had no withdraw ops".to_owned(),
        });
    }
    Ok(ZoneAtomicWithdrawSubmission {
        withdraws: info.withdraws.iter().map(|w| w.op.clone()).collect(),
        publish: PublishResult {
            tx: PendingTx::AtomicWithdraw(info),
        },
    })
}

/// Asks the node wallet service to sign a Mantle transaction for the requested
/// ZK keys.
async fn sign_tx_zk(
    node_url: &Url,
    tx: &MantleTx,
    public_keys: Vec<ZkPublicKey>,
) -> Result<ZkSignature, ZoneTestError> {
    let request_url =
        node_url
            .join("wallet/sign/zk")
            .map_err(|error| ZoneTestError::SignTransaction {
                message: error.to_string(),
            })?;
    let response: WalletSignTxZkResponseBody = CommonHttpClient::new(None)
        .post(
            request_url,
            &WalletSignTxZkRequestBody {
                tx_hash: tx.hash(),
                pks: public_keys,
            },
        )
        .await
        .map_err(|error| ZoneTestError::SignTransaction {
            message: error.to_string(),
        })?;

    Ok(response.sig)
}
