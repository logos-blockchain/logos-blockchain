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
    ZoneMessage,
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{
        Event, FinalizedOp, InscriptionId, InscriptionInfo, OrphanedTx, PublishResult, PublishedTx,
        SequencerChannelView, SequencerCheckpoint, SequencerConfig, SequencerHandle,
        TurnNotification, WithdrawArg, ZoneSequencer,
    },
};
use rand::{Rng as _, thread_rng};
use reqwest::Url;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::warn;

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

    fn remaining(self) -> Result<Duration, ZoneTestError> {
        self.timeout
            .checked_sub(self.started_at.elapsed())
            .ok_or(ZoneTestError::PublishTimeout)
    }
}

/// Runs the SDK sequencer event stream in the background and exposes events to
/// test code that needs to wait for a specific publish result.
pub fn start_sequencer_event_loop(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
    republish_orphans: bool,
) -> (
    JoinHandle<()>,
    tokio::sync::mpsc::Receiver<Event>,
    tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let checkpoint_rx = sequencer.subscribe_checkpoint();

    let task = tokio::spawn(async move {
        let mut events = sequencer.events();
        while let Some(event) = events.next().await {
            if let (true, Event::ChannelUpdate { orphaned, .. }) = (republish_orphans, &event) {
                republish_orphaned_inscriptions(&handle, orphaned).await;
            }

            drop(tx.send(event).await);
        }
    });

    (task, rx, checkpoint_rx)
}

/// Drives a competing-sequencer policy that re-publishes invalidated payloads
/// until they are either pending again or adopted on chain.
pub fn start_republish_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = sequencer.events();
        while let Some(event) = events.next().await {
            if let Event::ChannelUpdate { orphaned, .. } = event {
                republish_orphaned_inscriptions(&handle, &orphaned).await;
            }
        }
    })
}

async fn republish_orphaned_inscriptions(
    handle: &SequencerHandle<ZoneNodeHttpClient>,
    orphaned: &[OrphanedTx],
) {
    for entry in orphaned {
        let OrphanedTx::Inscription(inscription) = entry else {
            // Republish-by-payload helper doesn't handle bundles.
            continue;
        };

        if let Err(error) = handle.publish_message(inscription.payload.clone()).await {
            warn!(%error, "Failed to re-publish orphaned zone payload");
        }
    }
}

/// Drives a policy that republishes orphaned balance updates only when the
/// local canonical view can still apply the update without going negative.
pub fn start_balance_aware_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
    initial_balances: ZoneAccountBalances,
    planned_payloads: Vec<Inscription>,
) -> JoinHandle<()> {
    let view_rx = sequencer.subscribe_channel_view();
    tokio::spawn(async move {
        let mut balances = BalanceAwareState::new(initial_balances);
        let mut planned = VecDeque::from(planned_payloads);
        let mut events = sequencer.events();

        while let Some(event) = events.next().await {
            match event {
                Event::Published { tx, .. } => {
                    balances.record_applied_payload(&tx.inscription().payload);
                }
                Event::ChannelUpdate { orphaned, adopted } => {
                    let orphaned_inscriptions: Vec<InscriptionInfo> = orphaned
                        .into_iter()
                        .filter_map(|o| match o {
                            OrphanedTx::Inscription(i) => Some(i),
                            OrphanedTx::AtomicWithdraw(_) => None,
                        })
                        .collect();
                    balances.remove_orphaned_payloads(&orphaned_inscriptions);
                    balances.record_adopted_payloads(&adopted);
                    republish_affordable_balance_updates(
                        &handle,
                        &mut balances,
                        orphaned_inscriptions,
                    )
                    .await;
                }
                _ => {}
            }

            publish_planned_balance_updates(&handle, &view_rx, &mut balances, &mut planned).await;
        }
    })
}

/// Drives a deterministic conflict policy used by tests that expect the final
/// zone chain to converge to sorted payload order.
pub fn start_sorted_conflict_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
    discarded: DiscardedPayloads,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sorted_state = SortedConflictState::new(discarded);
        let mut events = sequencer.events();

        while let Some(event) = events.next().await {
            match event {
                Event::Published { tx, .. } => {
                    sorted_state
                        .record_published_payload(tx.inscription().payload.clone())
                        .await;
                }
                Event::ChannelUpdate { orphaned, adopted } => {
                    sorted_state.record_adoptions(&adopted).await;

                    for entry in orphaned {
                        let OrphanedTx::Inscription(inscription) = entry else {
                            // Sorted-conflict policy doesn't handle bundles.
                            continue;
                        };
                        if sorted_state.already_discarded(&inscription.payload).await {
                            continue;
                        }

                        if sorted_state.preserves_order(&inscription) {
                            if let Err(error) = handle.publish_message(inscription.payload).await {
                                warn!(%error, "Failed to re-publish sorted zone payload");
                            }
                        } else {
                            sorted_state.discard(inscription.payload.clone()).await;
                        }
                    }
                }
                _ => {}
            }
        }
    })
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

async fn republish_affordable_balance_updates(
    handle: &SequencerHandle<ZoneNodeHttpClient>,
    balances: &mut BalanceAwareState,
    orphaned: Vec<InscriptionInfo>,
) {
    for inscription in orphaned {
        if !balances.should_republish(&inscription.payload) {
            continue;
        }

        let payload = inscription.payload;
        if let Err(error) = handle.publish_message(payload.clone()).await {
            warn!(%error, "Failed to re-publish balance-aware zone payload");

            continue;
        }

        balances.record_republished_payload(&payload);
    }
}

async fn publish_planned_balance_updates(
    handle: &SequencerHandle<ZoneNodeHttpClient>,
    view_rx: &tokio::sync::watch::Receiver<SequencerChannelView>,
    balances: &mut BalanceAwareState,
    planned: &mut VecDeque<Inscription>,
) {
    if !view_rx.borrow().our_turn_to_write {
        return;
    }

    while let Some(payload) = planned.pop_front() {
        if !balances.should_republish(&payload) {
            continue;
        }

        if let Err(error) = handle.publish_message(payload.clone()).await {
            warn!(%error, "Failed to publish planned balance-aware zone payload");
            planned.push_front(payload);
            break;
        }

        balances.record_republished_payload(&payload);
    }
}

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

/// Publishes a zone payload and waits for the SDK to emit the matching
/// `Published` event, retrying transient publish failures until the deadline.
pub async fn publish_message_with_retry(
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    data: &Inscription,
    deadline: PublishDeadline,
) -> Result<PublishResult, ZoneTestError> {
    loop {
        if deadline.is_expired() {
            return Err(ZoneTestError::PublishTimeout);
        }

        match sequencer.publish_message(data.clone()).await {
            Ok(()) => return wait_for_published_event(sequencer_events, data, deadline).await,
            Err(error) => {
                warn!(error = %error, "Zone sequencer publish failed, retrying");

                sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn wait_for_published_event(
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    data: &[u8],
    deadline: PublishDeadline,
) -> Result<PublishResult, ZoneTestError> {
    timeout(deadline.remaining()?, async {
        while let Some(event) = sequencer_events.recv().await {
            if let Event::Published { tx } = event
                && tx.inscription().payload.as_slice() == data
            {
                return Ok(PublishResult {
                    inscription_id: tx.tx_hash(),
                });
            }
        }

        Err(ZoneTestError::PublishTimeout)
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
}

/// Waits for one payload to emit the matching `Published` event.
pub async fn wait_for_published_payload(
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    data: &[u8],
    duration: Duration,
) -> Result<PublishResult, ZoneTestError> {
    wait_for_published_event(sequencer_events, data, PublishDeadline::from_now(duration)).await
}

/// Waits for all listed payloads to emit matching `Published` events.
pub async fn wait_for_published_payloads(
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    data: &[Inscription],
    duration: Duration,
) -> Result<Vec<PublishResult>, ZoneTestError> {
    timeout(duration, async {
        let mut results: Vec<Option<PublishResult>> =
            std::iter::repeat_with(|| None).take(data.len()).collect();
        let mut remaining = data.len();

        while remaining > 0 {
            let Some(event) = sequencer_events.recv().await else {
                return Err(ZoneTestError::PublishTimeout);
            };
            let Event::Published { tx } = event else {
                continue;
            };
            let payload = tx.inscription().payload.as_slice();
            let Some(index) = data.iter().enumerate().find_map(|(index, expected)| {
                (results[index].is_none() && payload == expected.as_slice()).then_some(index)
            }) else {
                continue;
            };

            results[index] = Some(PublishResult {
                inscription_id: tx.tx_hash(),
            });
            remaining -= 1;
        }

        Ok(results.into_iter().flatten().collect())
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
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

/// Waits until the sequencer's [`Event::TxsFinalized`] stream surfaces the
/// expected deposit (matched by `inputs`, `amount`, and `metadata`). Drains
/// the events channel as it goes — call this after any earlier event
/// consumers in the scenario have moved past the relevant publish events.
pub async fn wait_for_finalized_deposit_via_sequencer(
    events: &mut tokio::sync::mpsc::Receiver<Event>,
    expected: &DepositOp,
    expected_amount: Value,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_sequencer_finalized_until(events, duration, ZoneTestError::IndexerTimeout, |op| {
        matches!(op, FinalizedOp::Deposit(d)
            if d.inputs == expected.inputs
                && d.amount == expected_amount
                && d.metadata == expected.metadata)
    })
    .await
}

/// Waits until the sequencer's [`Event::TxsFinalized`] stream surfaces the
/// expected withdraw (matched by `outputs`). Drains the events channel as
/// it goes.
pub async fn wait_for_finalized_withdraw_via_sequencer(
    events: &mut tokio::sync::mpsc::Receiver<Event>,
    expected: &ChannelWithdrawOp,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_sequencer_finalized_until(
        events,
        duration,
        ZoneTestError::WithdrawTimeout,
        |op| matches!(op, FinalizedOp::Withdraw(w) if w.op.outputs == expected.outputs),
    )
    .await
}

async fn poll_sequencer_finalized_until(
    events: &mut tokio::sync::mpsc::Receiver<Event>,
    duration: Duration,
    timeout_error: ZoneTestError,
    mut predicate: impl FnMut(&FinalizedOp) -> bool,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        while let Some(event) = events.recv().await {
            let Event::TxsFinalized { items } = event else {
                continue;
            };
            for tx in items {
                if tx.ops.iter().any(&mut predicate) {
                    return Ok(());
                }
            }
        }
        Err(ZoneTestError::SequencerStopped)
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
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
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

    let (tx, msg_id, sequencer_sig) = sequencer
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

    let result = sequencer
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
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
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

    let (tx, msg_id, inscription_sig) = sequencer
        .prepare_tx(
            [Op::ChannelWithdraw(withdraw.clone())].into(),
            inscription_data,
        )
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    let withdraw_sig =
        sequencer
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

    let result = sequencer
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

/// Publishes an atomic inscription+withdraw bundle via the
/// [`SequencerHandle::publish_atomic_withdraw`] API (fire-and-forget). Waits
/// for the matching `Event::Published` carrying the
/// [`PublishedTx::AtomicWithdraw`] variant and returns every withdraw op (with
/// the nonce filled by the SDK) so downstream cucumber assertions can match
/// each withdraw by its outputs.
///
/// `outputs_per_arg` carries one entry per `WithdrawArg`; each inner `Vec`
/// becomes that arg's `Outputs` (one `Note::new(amount, funding_pk)` per
/// listed amount). Exercises the SDK API at full width: multiple args, with
/// any arg able to carry multiple output notes.
pub async fn publish_atomic_zone_withdraw(
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    funding_public_key: ZkPublicKey,
    outputs_per_arg: Vec<Vec<Value>>,
    inscription_data: Inscription,
    deadline: PublishDeadline,
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

    sequencer
        .publish_atomic_withdraw(inscription_data.clone(), withdraw_args)
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    timeout(deadline.remaining()?, async {
        while let Some(event) = sequencer_events.recv().await {
            let Event::Published { tx } = event else {
                continue;
            };
            if tx.inscription().payload != inscription_data {
                continue;
            }
            let PublishedTx::AtomicWithdraw(info) = *tx else {
                // The sequencer may surface other Published events (e.g. a
                // plain inscription with a coincidental payload from a
                // concurrent flow). Skip and keep waiting for the bundle.
                warn!("ignoring non-AtomicWithdraw Published event while awaiting bundle");
                continue;
            };
            if info.withdraws.is_empty() {
                return Err(ZoneTestError::SubmitWithdraw {
                    message: "atomic withdraw bundle had no withdraw ops".to_owned(),
                });
            }
            let withdraws = info.withdraws.into_iter().map(|w| w.op).collect();
            return Ok(ZoneAtomicWithdrawSubmission {
                withdraws,
                publish: PublishResult {
                    inscription_id: info.tx_hash,
                },
            });
        }
        Err(ZoneTestError::SubmitWithdraw {
            message: "sequencer event channel closed before AtomicWithdraw published".to_owned(),
        })
    })
    .await
    .map_err(|_| ZoneTestError::SubmitWithdraw {
        message: "timed out waiting for atomic-withdraw Published event".to_owned(),
    })?
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
