//! Zone SDK test helpers shared by Cucumber steps.
//!
//! The helpers in this module keep the feature steps focused on scenario
//! intent: start a zone-backed node, run sequencers, publish messages, observe
//! the indexer, and submit the channel operations that the zone layer relies
//! on.

use std::{
    collections::{HashMap, HashSet},
    num::NonZero,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use lb_common_http_client::{CommonHttpClient, Slot};
use lb_config::consensus::{ProviderInfo, create_genesis_block_with_declarations};
use lb_core::{
    mantle::{
        GenesisTx as _, MantleTx, Note, NoteId, Op, OpProof, Transaction as _, Value,
        ledger::{Inputs, Outputs},
        ops::{
            channel::{
                ChannelId, deposit::DepositOp, inscribe::Inscription, withdraw::ChannelWithdrawOp,
            },
            transfer::TransferOp,
        },
    },
    proofs::channel_multi_sig_proof::{ChannelMultiSigProof, IndexedSignature},
    sdp::{Locator, ServiceType},
};
use lb_http_api_common::bodies::{
    channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
    wallet::{
        balance::WalletBalanceResponseBody,
        sign::{WalletSignTxZkRequestBody, WalletSignTxZkResponseBody},
    },
};
use lb_key_management_system_service::keys::{
    Ed25519Key, ZkPublicKey, ZkSignature, secured_key::SecuredKey as _,
};
use lb_node::{SignedMantleTx, config::RunConfig};
use lb_testing_framework::{
    DeploymentBuilder, LbcEnv, LbcLocalDeployer, LbcManualCluster, NodeHttpClient, TopologyConfig,
    internal::DeploymentPlan,
};
use lb_utils::math::NonNegativeRatio;
use lb_zone_sdk::{
    ZoneMessage,
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{
        Event, InscriptionId, OrphanedTx, PublishResult, SequencerConfig, SequencerHandle,
        WithdrawArg, ZoneSequencer,
    },
    state::{InscriptionInfo, PublishedTx},
};
use rand::{Rng as _, thread_rng};
use reqwest::Url;
use testing_framework_core::scenario::{DynError, StartNodeOptions, StartedNode};
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::warn;

use crate::{
    common::chain::wait_for_transactions_inclusion,
    cucumber::utils::{extract_child_dir_name, matching_child_dirs},
};

#[derive(Debug, thiserror::Error)]
pub enum ZoneTestError {
    #[error("failed to build zone deployment: {message}")]
    BuildDeployment { message: String },
    #[error("failed to start zone node: {message}")]
    StartNode { message: String },
    #[error("failed to resolve zone runtime dir: {message}")]
    RuntimeDir { message: String },
    #[error("zone network did not become ready: {message}")]
    NetworkReady { message: String },
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
    #[error("failed to fetch wallet balance while preparing a zone deposit: {message}")]
    WalletBalance { message: String },
    #[error("failed to find a funding note with value at least {min_value}")]
    MissingFundingNote { min_value: Value },
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
}

/// Prepared deployment resources for the single-node zone test cluster.
pub struct ZoneClusterTemplate {
    pub cluster: LbcManualCluster,
    pub funding_public_key: ZkPublicKey,
}

/// A started single-node zone chain plus the resolved runtime directory used
/// for logs, recovery files, and diagnostics.
pub struct StartedZoneNode {
    pub started_node: StartedNode<LbcEnv>,
    pub runtime_dir: PathBuf,
}

/// Result of an atomic deposit scenario where a deposit and zone inscription
/// are submitted as one Mantle transaction.
pub struct AtomicZoneDepositSubmission {
    pub deposit: DepositOp,
    pub publish: PublishResult,
}

/// Result of a withdraw scenario where the zone sequencer signs the channel
/// withdraw and publishes the accompanying inscription.
pub struct ZoneWithdrawSubmission {
    pub withdraw: ChannelWithdrawOp,
    pub publish: PublishResult,
}

struct SelectedNote {
    id: NoteId,
    value: Value,
}

pub type DiscardedPayloads = Arc<tokio::sync::Mutex<HashSet<Vec<u8>>>>;
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

/// Builds the manual-cluster template used by zone scenarios.
///
/// The generated genesis state includes the node funding key and Blend
/// declarations expected by the zone SDK tests. The caller still decides when
/// to start the node so Cucumber can keep cluster setup and runtime startup
/// visible as separate scenario phases.
pub fn prepare_zone_cluster(
    scenario_base_dir: PathBuf,
) -> Result<ZoneClusterTemplate, ZoneTestError> {
    let deployment = build_zone_deployment(scenario_base_dir)?;
    let funding_public_key = deployment.nodes()[0]
        .general
        .consensus_config
        .funding_sk
        .as_public_key();

    let cluster = LbcLocalDeployer::new().manual_cluster_from_descriptors(deployment);

    Ok(ZoneClusterTemplate {
        cluster,
        funding_public_key,
    })
}

/// Starts the zone node from a prepared manual cluster and resolves the actual
/// runtime directory created by the local deployer.
pub async fn start_zone_node(
    cluster: &LbcManualCluster,
    scenario_base_dir: &Path,
) -> Result<StartedZoneNode, ZoneTestError> {
    let node_name = "0";
    let persist_dir = scenario_base_dir.join("node-0");

    let runtime_dir_prefix = format!(
        "{}_",
        persist_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("node-0")
    );
    let ignore_list = matching_child_dirs(&persist_dir, &runtime_dir_prefix);

    let started_node = Box::pin(
        cluster.start_node_with(
            node_name,
            StartNodeOptions::default()
                .with_persist_dir(persist_dir)
                .create_patch(fast_zone_config_patch),
        ),
    )
    .await
    .map_err(|error| ZoneTestError::StartNode {
        message: error.to_string(),
    })?;

    let runtime_dir_name =
        extract_child_dir_name(scenario_base_dir, &runtime_dir_prefix, &ignore_list).map_err(
            |error| ZoneTestError::RuntimeDir {
                message: error.to_string(),
            },
        )?;

    Ok(StartedZoneNode {
        started_node,
        runtime_dir: scenario_base_dir.join(runtime_dir_name),
    })
}

/// Waits until the local node reports that its networking layer is ready for
/// the zone SDK to publish transactions through it.
pub async fn wait_for_zone_network_ready(cluster: &LbcManualCluster) -> Result<(), ZoneTestError> {
    cluster
        .wait_network_ready()
        .await
        .map_err(|error| ZoneTestError::NetworkReady {
            message: error.to_string(),
        })
}

/// Runs the SDK sequencer event stream in the background and exposes events to
/// test code that needs to wait for a specific publish result.
pub fn start_sequencer_event_loop(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
) -> (JoinHandle<()>, tokio::sync::mpsc::Receiver<Event>) {
    let (tx, rx) = tokio::sync::mpsc::channel(256);

    let handle = tokio::spawn(async move {
        loop {
            if let Some(event) = sequencer.next_event().await {
                drop(tx.send(event).await);
            }
        }
    });

    (handle, rx)
}

/// Drives a competing-sequencer policy that re-publishes this sequencer's own
/// invalidated payloads until they are either pending again or adopted on
/// chain.
pub fn start_republish_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut local_pending = HashSet::new();

        loop {
            match sequencer.next_event().await {
                Some(Event::Published { tx, .. }) => {
                    local_pending.insert(tx.inscription().this_msg);
                }
                Some(Event::TxsFinalized { txs, .. }) => {
                    for tx in txs {
                        local_pending.remove(&tx.inscription().this_msg);
                    }
                }
                Some(Event::FinalizedInscriptions { inscriptions }) => {
                    for inscription in inscriptions {
                        local_pending.remove(&inscription.this_msg);
                    }
                }
                Some(Event::ChannelUpdate { orphaned, .. }) => {
                    for entry in orphaned {
                        let OrphanedTx::Inscription(inscription) = entry else {
                            // Republish-by-payload helper doesn't handle bundles.
                            continue;
                        };
                        if !local_pending.remove(&inscription.this_msg) {
                            continue;
                        }

                        if let Err(error) = handle.publish_message(inscription.payload).await {
                            warn!(%error, "Failed to re-publish orphaned zone payload");
                        }
                    }
                }
                _ => {}
            }
        }
    })
}

/// Drives a policy that republishes orphaned balance updates only when the
/// local canonical view can still apply the update without going negative.
pub fn start_balance_aware_policy(
    mut sequencer: ZoneSequencer<ZoneNodeHttpClient>,
    handle: SequencerHandle<ZoneNodeHttpClient>,
    initial_balances: ZoneAccountBalances,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut balances = BalanceAwareState::new(initial_balances);

        loop {
            match sequencer.next_event().await {
                Some(Event::Published { tx, .. }) => {
                    balances.record_applied_payload(&tx.inscription().payload);
                }
                Some(Event::ChannelUpdate { orphaned, adopted }) => {
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

        loop {
            match sequencer.next_event().await {
                Some(Event::Published { tx, .. }) => {
                    sorted_state.record_seen_payload(tx.inscription().payload.clone().into_inner());
                }
                Some(Event::ChannelUpdate { orphaned, adopted }) => {
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
                            sorted_state.discard(inscription.payload.into_inner()).await;
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

    fn record_applied_payload(&mut self, payload: &[u8]) {
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

    fn should_republish(&self, payload: &[u8]) -> bool {
        let Some((uuid, account, delta)) = parse_balance_payload(payload) else {
            return false;
        };

        if self.account_updates(&account).contains_key(&uuid) {
            return false;
        }

        self.available_balance(&account) + delta >= 0
    }

    fn record_republished_payload(&mut self, payload: &[u8]) {
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

struct SortedConflictState {
    max_seen_on_chain: Option<Vec<u8>>,
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
            self.discarded
                .lock()
                .await
                .remove(payload.payload.as_slice());
            self.record_seen_payload(payload.payload.clone().into_inner());
        }
    }

    fn record_seen_payload(&mut self, payload: Vec<u8>) {
        if self
            .max_seen_on_chain
            .as_ref()
            .is_none_or(|seen| payload > *seen)
        {
            self.max_seen_on_chain = Some(payload);
        }
    }

    async fn already_discarded(&self, payload: &[u8]) -> bool {
        self.discarded.lock().await.contains(payload)
    }

    fn preserves_order(&self, inscription: &InscriptionInfo) -> bool {
        self.max_seen_on_chain
            .as_deref()
            .is_none_or(|seen| inscription.payload.as_slice() >= seen)
    }

    async fn discard(&self, payload: Vec<u8>) {
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
pub fn balance_update_payload(uuid: &str, account: &str, delta: i64) -> Vec<u8> {
    format!("{uuid}:{account}:{delta}").into_bytes()
}

/// Parses a balance-affecting payload in the same format produced by
/// [`balance_update_payload`].
pub fn parse_balance_payload(payload: &[u8]) -> Option<(String, String, i64)> {
    let payload = std::str::from_utf8(payload).ok()?;
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
        ..SequencerConfig::default()
    }
}

/// Publishes a zone payload and waits for the SDK to emit the matching
/// `Published` event, retrying transient publish failures until the deadline.
pub async fn publish_message_with_retry(
    sequencer: &SequencerHandle<ZoneNodeHttpClient>,
    sequencer_events: &mut tokio::sync::mpsc::Receiver<Event>,
    data: &[u8],
    deadline: PublishDeadline,
) -> Result<PublishResult, ZoneTestError> {
    loop {
        if deadline.is_expired() {
            return Err(ZoneTestError::PublishTimeout);
        }

        match sequencer
            .publish_message(Inscription::new_unchecked(data.to_vec()))
            .await
        {
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
            if let Event::Published { tx, checkpoint } = event
                && tx.inscription().payload.as_slice() == data
            {
                return Ok(PublishResult {
                    inscription_id: tx.tx_hash(),
                    checkpoint,
                });
            }
        }

        Err(ZoneTestError::PublishTimeout)
    })
    .await
    .map_err(|_| ZoneTestError::PublishTimeout)?
}

/// Collects indexed block payloads until all expected messages have appeared.
///
/// The returned order is the order observed from the indexer, which lets
/// assertions decide whether ordering matters for the scenario.
pub async fn collect_indexed_messages(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected_messages: &[Vec<u8>],
    duration: Duration,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let expected: HashSet<Vec<u8>> = expected_messages.iter().cloned().collect();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut ordered: Vec<Vec<u8>> = Vec::new();

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
    expected_messages: &[Vec<u8>],
    duration: Duration,
) -> Result<Vec<Vec<u8>>, ZoneTestError> {
    let expected: HashSet<Vec<u8>> = expected_messages.iter().cloned().collect();

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
                    let ZoneMessage::Block(block) = message else {
                        continue;
                    };

                    saw_message = true;
                    cursor = Some((block.id, slot));

                    if expected.contains(&block.data) {
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
    expected_payload: &[u8],
    expected_count: usize,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    timeout(duration, async {
        loop {
            let count = count_indexed_payload(indexer, expected_payload).await?;

            if count >= expected_count {
                sleep(Duration::from_secs(30)).await;

                let final_count = count_indexed_payload(indexer, expected_payload).await?;
                if final_count == expected_count {
                    return Ok(());
                }

                return Err(ZoneTestError::IndexedPayloadCountMismatch {
                    payload: String::from_utf8_lossy(expected_payload).to_string(),
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
    expected_payload: &[u8],
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
            let ZoneMessage::Block(block) = message else {
                continue;
            };

            saw_message = true;
            cursor = Some((block.id, slot));
            if block.data == expected_payload {
                count += 1;
            }
        }

        if !saw_message {
            return Ok(count);
        }
    }
}

/// Waits until the zone indexer observes the expected channel deposit.
pub async fn wait_for_deposit(
    indexer: &ZoneIndexer<ZoneNodeHttpClient>,
    expected: &DepositOp,
    duration: Duration,
) -> Result<(), ZoneTestError> {
    poll_zone_indexer_until(
        indexer,
        duration,
        || ZoneTestError::IndexerTimeout,
        |message| match message {
            ZoneMessage::Deposit(deposit)
                if deposit.inputs == expected.inputs
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
                if let ZoneMessage::Block(block) = &message {
                    cursor = Some((block.id, slot));
                }

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
pub async fn build_zone_deposit(
    node_url: &Url,
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    metadata: Vec<u8>,
) -> Result<DepositOp, ZoneTestError> {
    let note = get_note_with_exact_value(node_url, funding_public_key, amount).await?;

    Ok(DepositOp {
        channel_id,
        inputs: Inputs::new(vec![note.id]),
        metadata,
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
    channel_id: ChannelId,
    funding_public_key: ZkPublicKey,
    amount: Value,
    metadata: Vec<u8>,
    inscription_data: Vec<u8>,
) -> Result<AtomicZoneDepositSubmission, ZoneTestError> {
    let transfer = build_atomic_deposit_transfer(node_url, funding_public_key, amount).await?;
    let deposit = build_atomic_deposit_op(channel_id, metadata, &transfer)?;

    let (tx, msg_id, sequencer_sig) = sequencer
        .prepare_tx(
            [Op::Transfer(transfer), Op::ChannelDeposit(deposit.clone())].into(),
            Inscription::new_unchecked(inscription_data),
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
    })
}

/// Builds the funding transfer that creates the note consumed by an atomic
/// zone deposit.
async fn build_atomic_deposit_transfer(
    node_url: &Url,
    funding_public_key: ZkPublicKey,
    amount: Value,
) -> Result<TransferOp, ZoneTestError> {
    let funding_note = get_note_with_value(node_url, funding_public_key, amount).await?;
    let deposit_note = Note::new(amount, funding_public_key);
    let change = funding_note.value.checked_sub(amount).ok_or_else(|| {
        ZoneTestError::BuildAtomicDeposit {
            message: format!(
                "selected funding note value {} is below deposit amount {amount}",
                funding_note.value
            ),
        }
    })?;

    Ok(TransferOp {
        inputs: Inputs::new(vec![funding_note.id]),
        outputs: build_atomic_deposit_outputs(deposit_note, change, funding_public_key),
    })
}

/// Points the channel deposit at the note created by the atomic funding
/// transfer, keeping both operations in the same transaction.
fn build_atomic_deposit_op(
    channel_id: ChannelId,
    metadata: Vec<u8>,
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
        inputs: Inputs::new(vec![deposit_note_id]),
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
    inscription_data: Vec<u8>,
) -> Result<ZoneWithdrawSubmission, ZoneTestError> {
    let withdraw = ChannelWithdrawOp {
        channel_id,
        outputs: Outputs::new(vec![Note::new(amount, funding_public_key)]),
        withdraw_nonce: 0,
    };

    let (tx, msg_id, inscription_sig) = sequencer
        .prepare_tx(
            [Op::ChannelWithdraw(withdraw.clone())].into(),
            Inscription::new_unchecked(inscription_data),
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
    inscription_data: Vec<u8>,
    deadline: PublishDeadline,
) -> Result<ZoneAtomicWithdrawSubmission, ZoneTestError> {
    if outputs_per_arg.is_empty() {
        return Err(ZoneTestError::SubmitWithdraw {
            message: "publish_atomic_zone_withdraw requires at least one withdraw arg".to_owned(),
        });
    }
    let withdraw_args: Vec<WithdrawArg> = outputs_per_arg
        .iter()
        .map(|amounts| WithdrawArg {
            outputs: Outputs::new(
                amounts
                    .iter()
                    .map(|amount| Note::new(*amount, funding_public_key))
                    .collect(),
            ),
        })
        .collect();

    sequencer
        .publish_atomic_withdraw(
            Inscription::new_unchecked(inscription_data.clone()),
            withdraw_args,
        )
        .await
        .map_err(|error| ZoneTestError::SubmitWithdraw {
            message: error.to_string(),
        })?;

    timeout(deadline.remaining()?, async {
        while let Some(event) = sequencer_events.recv().await {
            let Event::Published { tx, checkpoint } = event else {
                continue;
            };
            if tx.inscription().payload.as_slice() != inscription_data {
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
                    checkpoint,
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

/// Selects a wallet note large enough to fund a zone operation.
async fn get_note_with_value(
    node_url: &Url,
    public_key: ZkPublicKey,
    min_value: Value,
) -> Result<SelectedNote, ZoneTestError> {
    let balance = get_wallet_balance(node_url, public_key).await?;

    balance
        .notes
        .into_iter()
        .find(|(_, value)| *value >= min_value)
        .map(|(id, value)| SelectedNote { id, value })
        .ok_or(ZoneTestError::MissingFundingNote { min_value })
}

/// Selects a wallet note that must be consumed directly by a deposit test.
async fn get_note_with_exact_value(
    node_url: &Url,
    public_key: ZkPublicKey,
    value: Value,
) -> Result<SelectedNote, ZoneTestError> {
    let balance = get_wallet_balance(node_url, public_key).await?;

    balance
        .notes
        .into_iter()
        .find(|(_, note_value)| *note_value == value)
        .map(|(id, value)| SelectedNote { id, value })
        .ok_or(ZoneTestError::MissingExactFundingNote { value })
}

/// Reads wallet-visible notes for the test funding key from the node API.
async fn get_wallet_balance(
    node_url: &Url,
    public_key: ZkPublicKey,
) -> Result<WalletBalanceResponseBody, ZoneTestError> {
    let request_url = node_url
        .join(&format!(
            "wallet/{}/balance",
            hex::encode(lb_groth16::fr_to_bytes(&public_key.into()))
        ))
        .map_err(|error| ZoneTestError::WalletBalance {
            message: error.to_string(),
        })?;

    CommonHttpClient::new(None)
        .get::<(), WalletBalanceResponseBody>(request_url, None)
        .await
        .map_err(|error| ZoneTestError::WalletBalance {
            message: error.to_string(),
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

/// Preserves leftover value from an atomic deposit funding note as wallet
/// change.
fn build_atomic_deposit_outputs(
    deposit_note: Note,
    change: Value,
    funding_public_key: ZkPublicKey,
) -> Outputs {
    if change > 0 {
        Outputs::new(vec![deposit_note, Note::new(change, funding_public_key)])
    } else {
        Outputs::new(vec![deposit_note])
    }
}

/// Builds the single-node deployment shape used by all migrated zone tests.
fn build_zone_deployment(scenario_base_dir: PathBuf) -> Result<DeploymentPlan, ZoneTestError> {
    let deployment = DeploymentBuilder::new(TopologyConfig::with_node_numbers(1))
        .scenario_base_dir(scenario_base_dir)
        .build()
        .map_err(|error| ZoneTestError::BuildDeployment {
            message: error.to_string(),
        })?;

    Ok(add_exact_deposit_notes_to_funding_key(
        deployment,
        [1, 3, 5],
    ))
}

/// Adds small exact-value notes that make deposit assertions deterministic
/// without setup transfers inside each scenario.
fn add_exact_deposit_notes_to_funding_key(
    mut deployment: DeploymentPlan,
    note_values: impl IntoIterator<Item = Value>,
) -> DeploymentPlan {
    let values = note_values.into_iter().collect::<Vec<_>>();

    if values.is_empty() {
        return deployment;
    }

    let funding_public_key = deployment.nodes()[0].general.consensus_config.funding_pk;
    let mut transfer_op = deployment
        .config
        .genesis_block
        .as_ref()
        .expect("zone deployment should include a genesis block")
        .genesis_tx()
        .genesis_transfer()
        .clone();

    transfer_op.outputs.as_mut().extend(
        values
            .into_iter()
            .map(|value| Note::new(value, funding_public_key)),
    );

    let providers = deployment
        .nodes()
        .iter()
        .take(deployment.config.blend_core_nodes)
        .map(|node| {
            let (blend_config, provider_sk, zk_sk) = &node.general.blend_config;

            ProviderInfo {
                service_type: ServiceType::BlendNetwork,
                provider_sk: provider_sk.clone(),
                zk_sk: zk_sk.clone(),
                locator: Locator::new_unchecked(
                    blend_config.core.backend.listening_address.clone(),
                ),
                note: node.general.consensus_config.blend_note.clone(),
            }
        })
        .collect();

    deployment.config.genesis_block = Some(create_genesis_block_with_declarations(
        transfer_op,
        providers,
        deployment.config.test_context.as_deref(),
    ));

    deployment
}

/// Shrinks consensus timing for zone Cucumber scenarios while keeping the node
/// otherwise close to the generated local deployment config.
fn fast_zone_config_patch(mut config: RunConfig) -> Result<RunConfig, DynError> {
    if config.user.api.backend.listen_address.port() == 0 {
        return Err("zone test config patch requires a non-zero API port".into());
    }

    config.deployment.time.slot_duration = Duration::from_secs(1);
    config.deployment.cryptarchia.slot_activation_coeff =
        NonNegativeRatio::new(1, 2.try_into().unwrap());

    config
        .user
        .cryptarchia
        .service
        .bootstrap
        .prolonged_bootstrap_period = Duration::ZERO;

    config.deployment.cryptarchia.security_param = NonZero::new(5).unwrap();

    Ok(config)
}
