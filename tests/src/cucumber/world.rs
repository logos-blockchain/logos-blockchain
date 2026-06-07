use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env,
    fmt::Debug,
    num::NonZero,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use cucumber::World;
use derivative::Derivative;
use lb_core::{
    codec::DeserializeOp as _,
    mantle::{
        SignedMantleTx, TxHash, Utxo, Value,
        ops::channel::{
            ChannelId, deposit::DepositOp, inscribe::Inscription, withdraw::ChannelWithdrawOp,
        },
    },
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, ZkPublicKey};
use lb_libp2p::{Multiaddr, PeerId};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    BlockFeed, LbcEnv, LbcK8sManualCluster, LbcManualCluster, NodeHttpClient, ScenarioBuilder,
    ScenarioBuilderExt as _, configs::wallet::WalletAccount, workloads,
};
use lb_zone_sdk::{
    adapter::NodeHttpClient as ZoneNodeHttpClient,
    indexer::ZoneIndexer,
    sequencer::{Event, InscriptionId, SequencerCheckpoint, SequencerHandle},
};
use reqwest::Url;
use testing_framework_core::scenario::{
    NodeControlCapability, PeerSelection, Scenario, StartedNode,
};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{
    BIN_PATH_RELEASE,
    common::wallet::{
        TrackedWalletKeysBySource, TrackedWallets, WalletBlockFeedTracker, WalletDiagnostics,
        WalletFeedTrackingBatch,
    },
    cucumber::{
        TARGET,
        defaults::{
            CUCUMBER_NODE_CONFIG_OVERRIDE, LOGOS_BLOCKCHAIN_NODE_BIN, init_node_log_dir_defaults,
            set_default_env,
        },
        error::{StepError, StepResult},
        fee_reserve::ScenarioFeeState,
        utils::{make_builder, shared_host_bin_path},
        wallet::feed::{CucumberWalletBlockFeed, CucumberWalletBlockFeedError},
    },
    non_zero,
};

type ScenarioBuilderWith = ScenarioBuilder;
type ConsensusLiveness = workloads::ConsensusLiveness;
pub type SharedTrackedWallets = Arc<Mutex<TrackedWallets>>;
pub type SharedWalletBlockFeedTracker = Arc<Mutex<WalletBlockFeedTracker>>;
pub type SharedScannedTransactionHashes = Arc<Mutex<HashSet<TxHash>>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeployerKind {
    #[default]
    Local,
    Compose,
    K8s,
}

impl DeployerKind {
    #[must_use]
    pub const fn uses_host_log_dir(self) -> bool {
        matches!(self, Self::Local | Self::K8s)
    }

    #[must_use]
    pub const fn requires_local_node_binary(self) -> bool {
        matches!(self, Self::Local)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkKind {
    Star,
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub result: Option<Result<(), String>>,
}

#[derive(Debug, Default, Clone)]
pub struct ManualNodeConfigOverrides {
    pub cryptarchia_security_param: Option<NonZero<u32>>,
    pub prolonged_bootstrap_period: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualClusterKind {
    Generated,
    Devnet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualClusterSpec {
    pub kind: ManualClusterKind,
    pub capacity: usize,
}

impl ManualNodeConfigOverrides {
    pub const fn apply_to(&self, config: &mut RunConfig) {
        if let Some(security_param) = self.cryptarchia_security_param {
            config.deployment.cryptarchia.security_param = security_param;
        }

        if let Some(prolonged_bootstrap_period) = self.prolonged_bootstrap_period {
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period = prolonged_bootstrap_period;
        }
    }

    pub const fn set_cryptarchia_security_param(&mut self, security_param: NonZero<u32>) {
        self.cryptarchia_security_param = Some(security_param);
    }

    pub const fn set_prolonged_bootstrap_period(&mut self, period: Duration) {
        self.prolonged_bootstrap_period = Some(period);
    }
}

pub struct ZonePublishedMessage {
    pub payload: Inscription,
    pub inscription_id: Option<InscriptionId>,
}

pub type ZoneDiscardedPayloads = Arc<tokio::sync::Mutex<HashSet<Inscription>>>;

pub struct ZoneSequencerIdentity {
    signing_key: Ed25519Key,
    channel_id: ChannelId,
    node_name: Option<String>,
    default_wallet_name: Option<String>,
}

pub struct ZoneSequencerRuntime {
    handle: SequencerHandle<ZoneNodeHttpClient>,
    task: JoinHandle<()>,
    events: Option<tokio::sync::mpsc::Receiver<Event>>,
    checkpoint_rx: Option<tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>>,
    ready_rx: tokio::sync::watch::Receiver<bool>,
    channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
    turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
    discarded_payloads: Option<ZoneDiscardedPayloads>,
}

#[derive(Clone, Copy, Default)]
pub struct ZoneSequencerStartup {
    pub pending_submit_depth: Option<usize>,
    pub passive_republish_orphans: bool,
}

#[derive(Default)]
pub struct ZoneState {
    node_name: Option<String>,
    indexer: Option<ZoneIndexer<ZoneNodeHttpClient>>,
    sequencers: HashMap<String, ZoneSequencerIdentity>,
    runtimes: HashMap<String, ZoneSequencerRuntime>,
    default_sequencer_alias: Option<String>,
    published_messages: HashMap<String, ZonePublishedMessage>,
    submitted_deposits: HashMap<String, (DepositOp, Value)>,
    submitted_withdraws: HashMap<String, ChannelWithdrawOp>,
    account_balances: HashMap<String, i64>,
    published_order: Vec<String>,
    saved_checkpoints: HashMap<String, SequencerCheckpoint>,
    latest_checkpoints: HashMap<String, SequencerCheckpoint>,
    sequencer_startups: HashMap<String, ZoneSequencerStartup>,
    sorted_total_payloads: Option<usize>,
    sorted_expected_by_sequencer: Option<HashMap<String, Vec<Inscription>>>,
}

impl ZoneState {
    pub fn clear(&mut self) {
        self.reset_zone_state();
    }

    pub fn node_name(&self) -> Result<&str, StepError> {
        self.node_name.as_deref().ok_or(StepError::LogicalError {
            message: "Zone cluster is not initialized".to_owned(),
        })
    }

    pub fn register_sequencer(&mut self, alias: String, signing_key: Ed25519Key) -> ChannelId {
        let channel_id = self.channel_for_new_sequencer(&signing_key);
        let existing_resources = self
            .sequencers
            .get(&alias)
            .map(|sequencer| {
                (
                    sequencer.node_name.clone(),
                    sequencer.default_wallet_name.clone(),
                )
            })
            .unwrap_or_default();

        self.sequencers.insert(
            alias.clone(),
            ZoneSequencerIdentity {
                signing_key,
                channel_id,
                node_name: existing_resources.0,
                default_wallet_name: existing_resources.1,
            },
        );

        if self.default_sequencer_alias.is_none() {
            self.default_sequencer_alias = Some(alias);
        }

        channel_id
    }

    fn channel_for_new_sequencer(&self, signing_key: &Ed25519Key) -> ChannelId {
        self.default_sequencer_alias
            .as_ref()
            .and_then(|alias| self.sequencers.get(alias))
            .map_or_else(
                || ChannelId::from(signing_key.public_key().to_bytes()),
                |sequencer| sequencer.channel_id,
            )
    }

    pub fn default_sequencer_alias(&self) -> Result<&str, StepError> {
        self.default_sequencer_alias
            .as_deref()
            .ok_or(StepError::LogicalError {
                message: "No zone sequencer is registered".to_owned(),
            })
    }

    pub fn sequencer_signing_key(&self, alias: &str) -> Result<&Ed25519Key, StepError> {
        self.sequencers
            .get(alias)
            .map(|sequencer| &sequencer.signing_key)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not registered"),
            })
    }

    pub fn default_sequencer_signing_key(&self) -> Result<&Ed25519Key, StepError> {
        let alias = self.default_sequencer_alias()?.to_owned();

        self.sequencer_signing_key(&alias)
    }

    pub fn sequencer_channel_id(&self, alias: &str) -> Result<ChannelId, StepError> {
        self.sequencers
            .get(alias)
            .map(|sequencer| sequencer.channel_id)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not registered"),
            })
    }

    pub fn default_channel_id(&self) -> Result<ChannelId, StepError> {
        let alias = self.default_sequencer_alias()?.to_owned();

        self.sequencer_channel_id(&alias)
    }

    #[must_use]
    pub fn has_sequencer(&self, alias: &str) -> bool {
        self.sequencers.contains_key(alias)
    }

    pub fn attach_sequencer_resources(
        &mut self,
        alias: &str,
        node_name: String,
        wallet_name: String,
    ) -> Result<(), StepError> {
        if self.node_name.is_none() {
            self.node_name = Some(node_name.clone());
        }

        let sequencer = self
            .sequencers
            .get_mut(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not registered"),
            })?;

        sequencer.node_name = Some(node_name);
        sequencer.default_wallet_name = Some(wallet_name);

        Ok(())
    }

    pub fn sequencer_node_name(&self, alias: &str) -> Result<&str, StepError> {
        let sequencer = self
            .sequencers
            .get(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not registered"),
            })?;

        sequencer
            .node_name
            .as_deref()
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not attached to a node"),
            })
    }

    pub fn sequencer_default_wallet_name(&self, alias: &str) -> Result<&str, StepError> {
        self.sequencers
            .get(alias)
            .and_then(|sequencer| sequencer.default_wallet_name.as_deref())
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' does not have a default wallet"),
            })
    }

    pub fn remember_zone_message(
        &mut self,
        alias: String,
        payload: Inscription,
        inscription_id: Option<InscriptionId>,
        sequencer_alias: Option<&str>,
        checkpoint: Option<SequencerCheckpoint>,
    ) {
        self.published_order.push(alias.clone());
        self.published_messages.insert(
            alias,
            ZonePublishedMessage {
                payload,
                inscription_id,
            },
        );

        if let (Some(sequencer_alias), Some(checkpoint)) = (sequencer_alias, checkpoint) {
            self.latest_checkpoints
                .insert(sequencer_alias.to_owned(), checkpoint);
        }
    }

    pub fn remember_submitted_deposit(&mut self, alias: String, deposit: DepositOp, amount: Value) {
        self.submitted_deposits.insert(alias, (deposit, amount));
    }

    pub fn resolve_submitted_deposit(
        &self,
        alias: impl AsRef<str>,
    ) -> Result<&(DepositOp, Value), StepError> {
        let alias = alias.as_ref();

        self.submitted_deposits
            .get(alias)
            .ok_or(StepError::LogicalError {
                message: format!("Zone deposit alias '{alias}' not found"),
            })
    }

    pub fn remember_submitted_withdraw(&mut self, alias: String, withdraw: ChannelWithdrawOp) {
        self.submitted_withdraws.insert(alias, withdraw);
    }

    pub fn resolve_submitted_withdraw(
        &self,
        alias: impl AsRef<str>,
    ) -> Result<&ChannelWithdrawOp, StepError> {
        let alias = alias.as_ref();

        self.submitted_withdraws
            .get(alias)
            .ok_or(StepError::LogicalError {
                message: format!("Zone withdraw alias '{alias}' not found"),
            })
    }

    pub fn set_zone_account_balances(&mut self, balances: HashMap<String, i64>) {
        self.account_balances = balances;
    }

    pub fn zone_account_balances(&self) -> Result<HashMap<String, i64>, StepError> {
        if self.account_balances.is_empty() {
            return Err(StepError::LogicalError {
                message: "Zone account balances are not initialized".to_owned(),
            });
        }

        Ok(self.account_balances.clone())
    }

    pub fn ordered_inscription_ids(&self) -> Result<Vec<InscriptionId>, StepError> {
        self.published_order
            .iter()
            .map(|alias| {
                self.published_messages
                    .get(alias)
                    .and_then(|message| message.inscription_id)
                    .ok_or(StepError::LogicalError {
                        message: format!(
                            "Zone message alias '{alias}' does not have a tracked inscription id"
                        ),
                    })
            })
            .collect()
    }

    pub fn message_payloads_for_aliases(
        &self,
        aliases: &[String],
    ) -> Result<Vec<Inscription>, StepError> {
        aliases
            .iter()
            .map(|alias| {
                self.published_messages
                    .get(alias)
                    .map(|message| message.payload.clone())
                    .ok_or(StepError::LogicalError {
                        message: format!("Zone message alias '{alias}' not found"),
                    })
            })
            .collect()
    }

    pub fn published_message_payloads(&self) -> Result<Vec<Inscription>, StepError> {
        self.message_payloads_for_aliases(&self.published_order)
    }

    #[must_use]
    pub const fn has_published_messages(&self) -> bool {
        !self.published_order.is_empty()
    }

    pub fn remember_checkpoint(&mut self, alias: String, checkpoint: SequencerCheckpoint) {
        self.saved_checkpoints.insert(alias, checkpoint);
    }

    pub fn set_latest_checkpoint_for(
        &mut self,
        sequencer_alias: &str,
        checkpoint: SequencerCheckpoint,
    ) {
        self.latest_checkpoints
            .insert(sequencer_alias.to_owned(), checkpoint);
    }

    pub fn set_sequencer_startup(
        &mut self,
        sequencer_alias: impl AsRef<str>,
        startup: ZoneSequencerStartup,
    ) {
        self.sequencer_startups
            .insert(sequencer_alias.as_ref().to_owned(), startup);
    }

    pub fn sequencer_startup_for(&self, sequencer_alias: impl AsRef<str>) -> ZoneSequencerStartup {
        self.sequencer_startups
            .get(sequencer_alias.as_ref())
            .copied()
            .unwrap_or_default()
    }

    pub fn current_checkpoint_for(
        &self,
        sequencer_alias: &str,
    ) -> Result<SequencerCheckpoint, StepError> {
        if let Some(Some(checkpoint)) = self
            .runtimes
            .get(sequencer_alias)
            .and_then(|runtime| runtime.checkpoint_rx.as_ref())
            .map(|rx| rx.borrow().clone())
        {
            return Ok(checkpoint);
        }

        self.latest_checkpoints
            .get(sequencer_alias)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Zone sequencer '{sequencer_alias}' has not produced a checkpoint yet"
                ),
            })
    }

    #[must_use]
    pub fn checkpoint_receiver(
        &self,
        sequencer_alias: &str,
    ) -> Option<tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>> {
        self.runtimes
            .get(sequencer_alias)
            .and_then(|runtime| runtime.checkpoint_rx.as_ref())
            .cloned()
    }

    pub fn resolve_checkpoint(
        &self,
        alias: impl AsRef<str>,
    ) -> Result<SequencerCheckpoint, StepError> {
        let alias = alias.as_ref();

        self.saved_checkpoints
            .get(alias)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Zone checkpoint alias '{alias}' not found"),
            })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "test-world runtime bundles many sequencer-owned receivers + state; \
                  introducing a wrapper type would just move the same fields around"
    )]
    pub fn set_sequencer_runtime(
        &mut self,
        alias: String,
        sequencer_handle: SequencerHandle<ZoneNodeHttpClient>,
        sequencer_task: JoinHandle<()>,
        sequencer_events: Option<tokio::sync::mpsc::Receiver<Event>>,
        checkpoint_rx: Option<tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>>,
        ready_rx: tokio::sync::watch::Receiver<bool>,
        channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
        turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
        discarded_payloads: Option<ZoneDiscardedPayloads>,
    ) {
        if let Some(runtime) = self.runtimes.remove(&alias) {
            runtime.task.abort();
        }

        self.runtimes.insert(
            alias,
            ZoneSequencerRuntime {
                handle: sequencer_handle,
                task: sequencer_task,
                events: sequencer_events,
                checkpoint_rx,
                ready_rx,
                channel_view_rx,
                turn_to_write_rx,
                discarded_payloads,
            },
        );
    }

    pub fn sequencer_ready_rx(
        &self,
        alias: &str,
    ) -> Result<tokio::sync::watch::Receiver<bool>, StepError> {
        self.runtimes
            .get(alias)
            .map(|runtime| runtime.ready_rx.clone())
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
            })
    }

    pub fn sequencer_channel_view_rx(
        &self,
        alias: &str,
    ) -> Result<tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>, StepError>
    {
        self.runtimes
            .get(alias)
            .map(|runtime| runtime.channel_view_rx.clone())
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
            })
    }

    pub fn sequencer_turn_to_write_rx(
        &self,
        alias: &str,
    ) -> Result<tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>, StepError>
    {
        self.runtimes
            .get(alias)
            .map(|runtime| runtime.turn_to_write_rx.clone())
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
            })
    }

    pub fn stop_sequencer(&mut self, alias: &str) -> Result<(), StepError> {
        let runtime = self.runtimes.remove(alias).ok_or(StepError::LogicalError {
            message: format!("Zone sequencer '{alias}' is not running"),
        })?;

        runtime.task.abort();

        Ok(())
    }

    pub fn sequencer_handle(
        &self,
        alias: &str,
    ) -> Result<&SequencerHandle<ZoneNodeHttpClient>, StepError> {
        self.runtimes
            .get(alias)
            .map(|runtime| &runtime.handle)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
            })
    }

    pub fn sequencer_events_mut(
        &mut self,
        alias: &str,
    ) -> Result<&mut tokio::sync::mpsc::Receiver<Event>, StepError> {
        self.runtimes
            .get_mut(alias)
            .and_then(|runtime| runtime.events.as_mut())
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' does not expose events"),
            })
    }

    pub fn discarded_payloads(&self, alias: &str) -> Result<ZoneDiscardedPayloads, StepError> {
        self.runtimes
            .get(alias)
            .and_then(|runtime| runtime.discarded_payloads.clone())
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' does not track discarded payloads"),
            })
    }

    pub const fn set_sorted_total_payloads(&mut self, total: usize) {
        self.sorted_total_payloads = Some(total);
    }

    pub fn set_sorted_expected_by_sequencer(
        &mut self,
        expected_by_sequencer: HashMap<String, Vec<Inscription>>,
    ) {
        self.sorted_expected_by_sequencer = Some(expected_by_sequencer);
    }

    pub fn sorted_total_payloads(&self) -> Result<usize, StepError> {
        self.sorted_total_payloads.ok_or(StepError::LogicalError {
            message: "Zone sorted conflict expectations are not initialized".to_owned(),
        })
    }

    pub fn sorted_expected_by_sequencer(
        &self,
    ) -> Result<HashMap<String, Vec<Inscription>>, StepError> {
        self.sorted_expected_by_sequencer
            .clone()
            .ok_or(StepError::LogicalError {
                message: "Zone sorted conflict payload order is not initialized".to_owned(),
            })
    }

    pub fn set_indexer(&mut self, indexer: ZoneIndexer<ZoneNodeHttpClient>) {
        self.indexer = Some(indexer);
    }

    pub fn indexer(&self) -> Result<&ZoneIndexer<ZoneNodeHttpClient>, StepError> {
        self.indexer.as_ref().ok_or(StepError::LogicalError {
            message: "Zone indexer is not initialized".to_owned(),
        })
    }

    #[must_use]
    pub fn debug_summary(&self) -> String {
        let node_name = self.node_name.as_deref().unwrap_or("<unset>");
        let sequencers = self.sequencers.len();
        let running = self.runtimes.len();
        let published = self.published_messages.len();
        let deposits = self.submitted_deposits.len();
        let withdraws = self.submitted_withdraws.len();
        let checkpoints = self.saved_checkpoints.len();

        format!(
            "node={node_name}, sequencers={sequencers}, running={running}, published={published}, deposits={deposits}, withdraws={withdraws}, checkpoints={checkpoints}"
        )
    }

    fn abort_all_runtimes(&mut self) {
        for (_, runtime) in self.runtimes.drain() {
            runtime.task.abort();
        }
    }

    fn reset_zone_state(&mut self) {
        self.abort_all_runtimes();

        self.node_name = None;
        self.indexer = None;
        self.default_sequencer_alias = None;
        self.sorted_total_payloads = None;
        self.sorted_expected_by_sequencer = None;

        self.sequencers.clear();
        self.published_messages.clear();
        self.submitted_deposits.clear();
        self.submitted_withdraws.clear();
        self.account_balances.clear();
        self.published_order.clear();
        self.saved_checkpoints.clear();
        self.latest_checkpoints.clear();
    }
}

#[derive(Debug, Clone)]
pub struct PublicCryptarchiaEndpointPeer {
    pub url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct ConfigOverride {
    /// Dot-separated user config path, e.g.
    /// `network.backend.swarm.gossipsub.retain_scores`.
    pub path: String,
    /// YAML value parsed from the step input.
    pub value: serde_yaml::Value,
}

#[derive(Debug, Default, Clone)]
pub struct ScenarioSpec {
    pub topology: Option<TopologySpec>,
    pub duration_secs: Option<NonZero<u64>>,
    pub wallets: Option<WalletSpec>,
    pub transactions: Option<TransactionSpec>,
    pub consensus_liveness: Option<ConsensusLivenessSpec>,
}

#[derive(Debug, Clone)]
pub struct TopologySpec {
    pub nodes: NonZero<usize>,
    pub network: NetworkKind,
    pub scenario_base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub struct WalletSpec {
    pub total_funds: u64,
    pub users: NonZero<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct TransactionSpec {
    pub rate_per_block: NonZero<u64>,
    pub users: Option<NonZero<usize>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsensusLivenessSpec {
    pub lag_allowance: Option<NonZero<u64>>,
}

#[derive(World, Derivative)]
#[derivative(Default)]
pub struct CucumberWorld {
    /// The deployer kind that this scenario is configured for.
    pub deployer: Option<DeployerKind>,
    /// A unique per-scenario context string used to isolate runtime resources.
    pub test_context: Option<String>,
    /// Base directory for scenario artifacts like logs and generated configs.
    pub scenario_base_dir: PathBuf,
    /// Automated: Scenario specification
    pub spec: ScenarioSpec,
    /// Automated: Runtime state for the scenario.
    pub run: RunState,
    /// Automated: Whether to perform membership checks on nodes after starting
    /// them, to verify they have joined the network as expected.
    pub membership_check: bool,
    /// Automated: Whether to perform readiness checks on nodes after starting
    /// them.
    pub readiness_checks: bool,
    /// Manual: List of genesis block UTXOs allocated in the genesis
    /// configuration.
    pub genesis_block_utxos: Vec<Utxo>,
    /// Manual: Optional local cluster instance for scenarios that use the local
    /// deployer.
    #[derivative(Default(value = "None"))]
    /// Manual: Mapping of logical node names to their corresponding node
    /// information, which includes the started node instance and any relevant
    /// metadata.
    pub local_cluster: Option<LbcManualCluster>,
    /// Manual: Optional k8s manual cluster instance for scenarios that use the
    /// k8s deployer.
    pub k8s_manual_cluster: Option<LbcK8sManualCluster>,
    /// Manual: List of nodes with their info.
    pub nodes_info: HashMap<String, NodeInfo>,
    /// Manual: List of genesis tokens allocated to wallets accounts.
    pub genesis_tokens: Vec<GenesisTokens>,
    /// Manual: Mapping of logical wallet names to their corresponding
    /// wallet resources.
    pub wallet_info: WalletInfoMap,
    /// Manual: Mapping of wallet account indices to their corresponding wallet
    /// account in the cluster.
    pub wallet_accounts: HashMap<usize, WalletAccount>,
    /// Manual: Scenario-level fee sponsor configuration and accounting.
    pub fee_state: ScenarioFeeState,
    /// Manual: Scenario-local wallet read model.
    ///
    /// Chain-derived UTXOs are tracked by the wallet block-feed tracker.
    /// Step code still records local intent, such as reserved inputs and
    /// submitted transaction hashes, directly in this state.
    pub wallets: SharedTrackedWallets,
    /// Manual: Runtime block feed used by wallet observation in manual
    /// scenarios.
    pub wallet_block_feed: Option<CucumberWalletBlockFeed>,
    /// Manual: Per-source wallet state advanced from the block feed.
    pub wallet_feed_tracker: SharedWalletBlockFeedTracker,
    /// Manual: Mapping of scenario transaction aliases to submitted hashes.
    pub submitted_transactions: HashMap<String, TxHash>,
    /// Manual: Exact signed transactions prepared for later submission.
    pub prepared_transactions: HashMap<String, SignedMantleTx>,
    /// Manual: Transaction hashes observed while wallet/block sync scanned
    /// blocks.
    pub scanned_transaction_hashes: SharedScannedTransactionHashes,
    /// Manual: Mapping of logical node names to their corresponding libp2p peer
    /// IDs.
    pub node_peer_ids: HashMap<String, PeerId>,
    /// Manual: `group_name` -> set of `node_names`. Empty means "no groups
    /// defined" and all nodes participate.
    pub node_groups: HashMap<String, BTreeSet<String>>,
    /// Manual: `node_name` -> `group_name` reverse lookup.
    pub node_to_group: HashMap<String, String>,
    /// Manual: Number of leading nodes declared as blend providers in the
    /// generated deployment. Defaults to all nodes when unset.
    pub blend_core_nodes: Option<usize>,
    /// Manual: Pending manual-cluster build recipe used to rebuild the local
    /// cluster when deployment-shape steps change before any nodes start.
    pub manual_cluster_spec: Option<ManualClusterSpec>,
    /// Manual: Whether to populate the IBD peers for each node after starting
    /// them,
    pub populate_ibd_peers_from_initial_peers: Option<bool>,
    /// Manual: Whether to require all peers to be online after starting them.
    pub require_all_peers_mode_online_at_startup: Option<Duration>,
    /// Manual: Initial peers (multiaddrs) injected into node config before
    /// start.
    pub initial_peers_override: Option<Vec<Multiaddr>>,
    /// Manual: IBD peers injected into node config before start.
    pub ibd_peers_override: Option<HashSet<PeerId>>,
    /// Manual: Public base endpoints and credentials used to query
    /// `/cryptarchia/info` for external chain sync reference.
    pub public_cryptarchia_endpoint_peers: Option<Vec<PublicCryptarchiaEndpointPeer>>,
    /// Manual: Dynamic user-config overrides applied on node startup.
    pub user_config_overrides: Vec<ConfigOverride>,
    /// Manual: Dynamic deployment-config overrides applied on node startup.
    pub deployment_config_overrides: Vec<ConfigOverride>,
    /// Manual: If set, nodes use a `DeploymentSettings` loaded from disk
    /// bypassing generated genesis/test deployment.
    pub deployment_config_override_path: Option<PathBuf>,
    /// Manual: If set, all running nodes are copied into a named snapshot when
    /// the scenario stops them.
    pub blockchain_snapshot_name_on_stop: Option<String>,
    /// Manual: If set, dynamically started nodes should initialize their chain
    /// state from this named snapshot. This is a scenario-wide startup seeding
    /// setting.
    pub blockchain_snapshot_on_startup: Option<NodeSnapshot>,
    /// Manual: Whether to have dynamically started nodes join the external
    /// network
    pub join_external_network: Option<bool>,
    /// Manual: Runtime state for node-control extensions added outside the
    /// legacy generic step files.
    pub manual_node_config_overrides: ManualNodeConfigOverrides,
    /// Manual: Faucet base URL configuration for manual transactions, if
    /// applicable.
    pub faucet_base_url: Option<String>,
    /// Manual: Faucet username configuration for manual transactions, if
    /// applicable.
    pub faucet_username: Option<String>,
    /// Manual: Faucet password configuration for manual transactions, if
    /// applicable.
    pub faucet_password: Option<String>,
    /// Manual: Task handles for dynamically spawned faucet funding tasks.
    #[derivative(Default(value = "None"))]
    pub faucet_task_handles: Option<Vec<JoinHandle<()>>>,
    /// Manual: Zone-specific state for SDK/sequencer scenarios.
    pub zone: ZoneState,
}

impl Drop for CucumberWorld {
    fn drop(&mut self) {
        self.zone.clear();

        if let Some(handles) = self.faucet_task_handles.take() {
            for handle in handles {
                handle.abort();
            }
        }
    }
}

/// Information about a node snapshot, which can be used to initialize
/// dynamically
#[derive(Debug, Default, Clone)]
pub struct NodeSnapshot {
    /// Logical name of the snapshot, used for referencing in steps.
    pub name: String,
    /// The node name that this snapshot corresponds to. This is used to
    /// determine which node's data directory will be used.
    pub node: String,
}

impl Debug for CucumberWorld {
    #[expect(
        clippy::too_many_lines,
        reason = "Debug output intentionally enumerates world state fields for test diagnostics"
    )]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wallet_diagnostics = self.wallet_diagnostics_for_debug().ok();
        let wallet_utxo_snapshot_count = wallet_diagnostics
            .as_ref()
            .map_or(0, |diagnostics| diagnostics.utxo_snapshot_count);
        let wallet_pending_count = wallet_diagnostics
            .as_ref()
            .map_or(0, |diagnostics| diagnostics.pending_wallet_count);
        let wallet_header_height_node_count = wallet_diagnostics
            .as_ref()
            .map_or(0, |diagnostics| diagnostics.header_height_node_count);

        f.debug_struct("CucumberWorld")
            .field("deployer", &format!("{:?}", self.deployer))
            .field("test_context", &format!("{:?}", self.test_context))
            .field("scenario_base_dir", &self.scenario_base_dir)
            .field("spec", &format!("{:?}", self.spec))
            .field("run", &format!("{:?}", self.run))
            .field("membership_check", &self.membership_check)
            .field("readiness_checks", &self.readiness_checks)
            .field(
                "join_external_network",
                &format!("{:?}", self.join_external_network),
            )
            .field(
                "populate_ibd_peers",
                &format!("{:?}", self.populate_ibd_peers_from_initial_peers),
            )
            .field(
                "require_all_peers_mode_online_at_startup",
                &format!("{:?}", self.require_all_peers_mode_online_at_startup),
            )
            .field(
                "genesis_block_utxos",
                &format!("{:?}", self.genesis_block_utxos),
            )
            .field("local_cluster", {
                if self.local_cluster.is_some() {
                    &"Has LbcManualCluster"
                } else {
                    &"None"
                }
            })
            .field("k8s_manual_cluster", {
                if self.k8s_manual_cluster.is_some() {
                    &"Has LbcK8sManualCluster"
                } else {
                    &"None"
                }
            })
            .field("nodes_info", &self.nodes_info.len())
            .field("genesis_tokens", &self.genesis_tokens.len())
            .field("wallet_info", &self.wallet_info.len())
            .field("faucet_base_url", &format!("{:?}", self.faucet_base_url))
            .field("faucet_username", &format!("{:?}", self.faucet_username))
            .field("faucet_password", &format!("{:?}", self.faucet_password))
            .field(
                "faucet_task_handles",
                &format!("{}", self.faucet_task_handles.as_ref().map_or(0, Vec::len)),
            )
            .field("wallet_accounts", &self.wallet_accounts.len())
            .field("scenario_fee_state", &fee_state_summary(&self.fee_state))
            .field("wallets", &"SharedTrackedWallets")
            .field("wallet_block_feed", &self.wallet_block_feed.is_some())
            .field("wallet_feed_tracker", &"WalletBlockFeedTracker")
            .field("submitted_transactions", &self.submitted_transactions.len())
            .field("prepared_transactions", &self.prepared_transactions.len())
            .field(
                "scanned_transaction_hashes",
                &self.scanned_transaction_hashes_len(),
            )
            .field("wallet_utxos_by_block", &wallet_utxo_snapshot_count)
            .field("wallet_pending_states", &wallet_pending_count)
            .field(
                "scenario_fee_encumbered_tokens",
                &self.fee_state.reserved_wallet_count(),
            )
            .field("node_header_heights", &wallet_header_height_node_count)
            .field("node_peer_ids", &self.node_peer_ids.len())
            .field("node_groups", &self.node_groups.len())
            .field("node_to_group", &self.node_to_group.len())
            .field("blend_core_nodes", &self.blend_core_nodes)
            .field("manual_cluster_spec", &self.manual_cluster_spec)
            .field(
                "manual_node_config_overrides",
                &self.manual_node_config_overrides,
            )
            .field("zone", &self.zone.debug_summary())
            .field(
                "initial_override_peers_display",
                &initial_peers_override_display(self.initial_peers_override.as_ref()),
            )
            .field(
                "ibd_peers_override_display",
                &ibd_peers_override_display(self.ibd_peers_override.as_ref()),
            )
            .field(
                "public_cryptarchia_endpoint_peers",
                &public_cryptarchia_endpoint_peers_display(
                    self.public_cryptarchia_endpoint_peers.as_ref(),
                ),
            )
            .field(
                "user_config_overrides",
                &user_config_overrides_display(&self.user_config_overrides),
            )
            .field(
                "deployment_config_overrides",
                &user_config_overrides_display(&self.deployment_config_overrides),
            )
            .field(
                "deployment_config_override_path",
                &deployment_config_override_path_display(
                    self.deployment_config_override_path.as_ref(),
                ),
            )
            .field(
                "blockchain_snapshot_name_on_stop",
                &self.blockchain_snapshot_name_on_stop,
            )
            .field(
                "blockchain_snapshot_name_on_startup",
                &blockchain_snapshot_on_startup_display(
                    self.blockchain_snapshot_on_startup.as_ref(),
                ),
            )
            .finish()
    }
}

/// Information about genesis tokens allocated to a wallet account in the world.
#[derive(Clone, Debug)]
pub struct GenesisTokens {
    /// The account index in the genesis tokens that this allocation corresponds
    /// to.
    pub account_index: usize,
    /// The number of tokens allocated to this account in the genesis
    /// configuration.
    pub token_count: usize,
    /// The total amount of tokens allocated to this account in the genesis
    /// configuration.
    pub token_amount: u64,
}

/// The wallet type can either be ussr defined or funding.
#[derive(Clone, Debug)]
pub enum WalletType {
    /// User defined wallets with are not tied to a specific node
    User { wallet_account: WalletAccount },
    /// Funding wallets are tied to a node and participate in consensus
    Funding { wallet_pk: String },
}

/// Information about a wallet resource created in the world, which can be used
/// to track and reference wallets across steps.
#[derive(Clone, Debug)]
pub struct WalletInfo {
    /// Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    /// Logical name of the node resource where this wallet is referenced.
    pub node_name: String,
    /// The wallet type, which can be either a user-defined wallet or a funding
    /// wallet.
    pub wallet_type: WalletType,
}

impl WalletInfo {
    /// Helper to get the wallet's public key as `String` type (default hex).
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        match &self.wallet_type {
            WalletType::User { wallet_account, .. } => wallet_account.public_key_hex(),
            WalletType::Funding { wallet_pk } => wallet_pk.clone(),
        }
    }

    /// Helper to get the wallet's public key as a `ZkPublicKey` type.
    pub fn public_key(&self) -> Result<ZkPublicKey, StepError> {
        match &self.wallet_type {
            WalletType::User { wallet_account, .. } => Ok(wallet_account.public_key()),
            WalletType::Funding { wallet_pk } => {
                Ok(ZkPublicKey::from_bytes(&hex::decode(wallet_pk)?)?)
            }
        }
    }

    /// Helper to determine if this wallet is a user-defined wallet.
    #[must_use]
    pub const fn is_user_wallet(&self) -> bool {
        matches!(self.wallet_type, WalletType::User { .. })
    }

    /// Helper to determine if this wallet is a funding wallet.
    #[must_use]
    pub const fn is_funding_wallet(&self) -> bool {
        matches!(self.wallet_type, WalletType::Funding { .. })
    }
}

/// Mapping of chain height to the corresponding block hash at that height.
pub type ChainInfoMap = HashMap<u64, String>;
/// Mapping of logical wallet names to their corresponding wallet
/// information.
pub type WalletInfoMap = HashMap<String, WalletInfo>;

/// Information about a started node in the world
pub struct NodeInfo {
    /// Node name
    pub name: String,
    /// The actual started node instance
    pub started_node: StartedNode<LbcEnv>,
    /// General node configuration used to start the node
    pub run_config: Option<RunConfig>,
    /// Chain height vs. hash at that height
    pub chain_info: ChainInfoMap,
    /// The wallets associated with this node.
    pub wallet_info: WalletInfoMap,
    /// The node's runtime directory where all its runtime artifacts will be
    /// collected
    pub runtime_dir: PathBuf,
    /// Whether this node is only expected to be network ready after startup and
    /// not `Mode::OnLine`
    pub immediate_start: bool,
}

impl NodeInfo {
    /// Convenience: record a node's current tip at its current height.
    pub fn upsert_tip(&mut self, height: u64, tip_hash_hex: String) {
        self.chain_info.insert(height, tip_hash_hex);
    }

    /// Returns the highest height for which we have a cached hash (if any).
    #[must_use]
    pub fn best_height(&self) -> Option<u64> {
        self.chain_info.keys().copied().max()
    }

    /// Returns a reference to the full map of cached height -> hash.
    #[must_use]
    pub const fn chain_info(&self) -> &ChainInfoMap {
        &self.chain_info
    }
}

impl CucumberWorld {
    /// Set a scenario-wide cryptarchia security parameter override for
    /// manual-cluster nodes.
    pub const fn set_cryptarchia_security_param(&mut self, security_param: NonZero<u32>) {
        self.manual_node_config_overrides
            .set_cryptarchia_security_param(security_param);
    }

    /// Set a scenario-wide prolonged bootstrap period override for
    /// manual-cluster nodes.
    pub const fn set_prolonged_bootstrap_period(&mut self, period: Duration) {
        self.manual_node_config_overrides
            .set_prolonged_bootstrap_period(period);
    }

    /// Get the best known height for the given node, if any. This is based on
    /// the cached height -> hash information stored in the world for each
    /// node.
    pub fn node_best_height(&self, node_name: &String) -> Result<Option<u64>, StepError> {
        let node = self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Runtime node '{node_name}' not found"),
            })?;
        Ok(node.best_height())
    }

    /// Set the deployer kind for this scenario.
    pub const fn set_deployer(&mut self, deployer: DeployerKind) {
        self.deployer = Some(deployer);
    }

    /// Set the directory where scenario artifacts should be stored.
    pub fn set_scenario_base_dir(&mut self, log_dir: &Path, deployer: &DeployerKind) {
        let log_dir = PathBuf::from(log_dir);
        init_node_log_dir_defaults(deployer, Some(&log_dir));

        self.scenario_base_dir.clone_from(&log_dir);
        if let Some(topology) = self.spec.topology.as_mut() {
            topology.scenario_base_dir = log_dir;
        }
    }

    pub fn set_test_context(&mut self, test_context: String) {
        self.test_context = Some(test_context);
    }

    /// Remove all scenario artifacts from the scenario base directory. This is
    /// useful for ensuring a clean state before starting a new scenario.
    pub fn clear_scenario_artifacts(&self) -> StepResult {
        if self.scenario_base_dir.is_dir() {
            std::fs::remove_dir_all(&self.scenario_base_dir).map_err(|e| {
                StepError::LogicalError {
                    message: format!(
                        "Failed to clear scenario artifacts in '{}': {e}",
                        self.scenario_base_dir.display()
                    ),
                }
            })?;
        }
        Ok(())
    }

    pub fn with_wallets<R>(
        &self,
        action: impl FnOnce(&TrackedWallets) -> R,
    ) -> Result<R, StepError> {
        let wallets = self.wallets.lock().map_err(|_| wallet_state_lock_error())?;

        Ok(action(&wallets))
    }

    pub fn with_wallets_mut<R>(
        &self,
        action: impl FnOnce(&mut TrackedWallets) -> R,
    ) -> Result<R, StepError> {
        let mut wallets = self.wallets.lock().map_err(|_| wallet_state_lock_error())?;

        Ok(action(&mut wallets))
    }

    pub fn with_wallet_feed_state_mut<R>(
        &self,
        action: impl FnOnce(&mut WalletBlockFeedTracker, &mut TrackedWallets) -> R,
    ) -> Result<R, StepError> {
        let mut tracker = self
            .wallet_feed_tracker
            .lock()
            .map_err(|_| wallet_feed_tracker_lock_error())?;
        let mut wallets = self.wallets.lock().map_err(|_| wallet_state_lock_error())?;

        Ok(action(&mut tracker, &mut wallets))
    }

    fn wallet_diagnostics_for_debug(&self) -> Result<WalletDiagnostics, StepError> {
        self.with_wallets(TrackedWallets::diagnostics)
    }

    pub async fn ensure_wallet_block_feed(&mut self) -> StepResult {
        if self.wallet_block_feed.is_some() {
            return Ok(());
        }

        let feed = CucumberWalletBlockFeed::start(
            Arc::clone(&self.wallets),
            Arc::clone(&self.wallet_feed_tracker),
            Arc::clone(&self.scanned_transaction_hashes),
            self.genesis_block_utxos.clone(),
        )
        .await
        .map_err(|error| wallet_block_feed_error(&error))?;

        for (node_name, node_info) in &self.nodes_info {
            feed.register_source(node_name, node_info.started_node.client.clone())
                .map_err(|error| wallet_block_feed_error(&error))?;
        }

        self.wallet_block_feed = Some(feed);
        self.track_known_wallets_with_block_feed()?;

        Ok(())
    }

    pub fn register_wallet_block_feed_source(
        &self,
        node_name: &str,
        client: NodeHttpClient,
    ) -> StepResult {
        let Some(feed) = &self.wallet_block_feed else {
            return Ok(());
        };

        feed.register_source(node_name, client)
            .map_err(|error| wallet_block_feed_error(&error))?;
        self.track_known_wallets_with_block_feed()
    }

    pub fn wallet_block_feed(&self) -> Result<BlockFeed, StepError> {
        self.wallet_block_feed
            .as_ref()
            .map(CucumberWalletBlockFeed::feed)
            .ok_or_else(|| StepError::LogicalError {
                message: "Wallet block feed is not running".to_owned(),
            })
    }

    pub fn reset_wallet_block_feed(&mut self) {
        self.wallet_block_feed = None;
        self.wallet_feed_tracker = Arc::new(Mutex::new(WalletBlockFeedTracker::default()));
    }

    /// Register currently known, unambiguous wallets with their owning feed
    /// source.
    ///
    /// This makes the feed tracker keep wallet state current in the
    /// background without letting unrelated fork sources overwrite the same
    /// wallet. Legacy sync-style steps can still add best-node tracking
    /// explicitly when needed.
    pub fn track_known_wallets_with_block_feed(&self) -> StepResult {
        if self.wallet_block_feed.is_none()
            || self.nodes_info.is_empty()
            || self.wallet_info.is_empty()
        {
            return Ok(());
        }

        let tracking_batches = self.known_wallet_tracking_batches()?;
        if tracking_batches.is_empty() {
            return Ok(());
        }

        let genesis_utxos = self.genesis_block_utxos.clone();
        let tracking = self
            .with_wallet_feed_state_mut(|tracker, wallets| {
                tracker.track_wallets(wallets, &tracking_batches, &genesis_utxos)
            })?
            .map_err(|error| StepError::LogicalError {
                message: error.to_string(),
            })?;

        if tracking.needs_backfill() {
            warn!(
                target: TARGET,
                "Wallet block feed tracking needs direct backfill for {} source tracker(s); \
                the next wallet observation will rebuild them",
                tracking.backfill_batches().len(),
            );
        }

        Ok(())
    }

    fn known_wallet_tracking_batches(&self) -> Result<Vec<WalletFeedTrackingBatch>, StepError> {
        let wallets_by_source = self.wallets_by_source_with_unique_public_keys()?;
        if wallets_by_source.is_empty() {
            return Ok(Vec::new());
        }

        let mut tracking_batches = Vec::new();
        for (source_node_name, wallets) in wallets_by_source {
            if !self.nodes_info.contains_key(&source_node_name) {
                continue;
            }

            let mut wallet_keys = TrackedWalletKeysBySource::new();
            for (wallet_name, public_key) in wallets {
                wallet_keys.add_wallet(&source_node_name, wallet_name, public_key);
            }

            tracking_batches.extend(wallet_keys.batches().map(|source_wallet_keys| {
                WalletFeedTrackingBatch::new(
                    source_node_name.clone(),
                    source_wallet_keys.wallet_keys().iter().cloned(),
                )
            }));
        }

        Ok(tracking_batches)
    }

    fn wallets_by_source_with_unique_public_keys(
        &self,
    ) -> Result<HashMap<String, Vec<(String, ZkPublicKey)>>, StepError> {
        let mut wallets_by_source_and_key: HashMap<String, HashMap<ZkPublicKey, Vec<String>>> =
            HashMap::new();

        for wallet in self.wallet_info.values() {
            wallets_by_source_and_key
                .entry(wallet.node_name.clone())
                .or_default()
                .entry(wallet.public_key()?)
                .or_default()
                .push(wallet.wallet_name.clone());
        }

        let mut wallets_by_source = HashMap::new();
        for (source_node_name, wallets_by_public_key) in wallets_by_source_and_key {
            for (public_key, wallet_names) in wallets_by_public_key {
                if let [wallet_name] = wallet_names.as_slice() {
                    wallets_by_source
                        .entry(source_node_name.clone())
                        .or_insert_with(Vec::new)
                        .push((wallet_name.clone(), public_key));
                } else {
                    warn!(
                        target: TARGET,
                        "Skipping automatic wallet feed tracking for aliases on `{}` with the same public key: {}",
                        source_node_name,
                        wallet_names.join(", ")
                    );
                }
            }
        }

        Ok(wallets_by_source)
    }

    /// Configure the scenario topology (number of nodes and network layout).
    pub fn set_topology(&mut self, nodes: usize, network: NetworkKind) -> StepResult {
        self.spec.topology = Some(TopologySpec {
            nodes: non_zero!("nodes", nodes)?,
            network,
            scenario_base_dir: self.scenario_base_dir.clone(),
        });
        Ok(())
    }

    /// Configure the scenario run duration in seconds.
    pub fn set_run_duration(&mut self, seconds: u64) -> StepResult {
        self.spec.duration_secs = Some(non_zero!("duration", seconds)?);
        Ok(())
    }

    // Configure the scenario wallets with total funds and number of users.
    pub fn set_wallets(&mut self, total_funds: u64, users: usize) -> StepResult {
        self.spec.wallets = Some(WalletSpec {
            total_funds,
            users: non_zero!("wallet users", users)?,
        });
        Ok(())
    }

    /// Configure the scenario transactions with a rate per block and optional
    /// number of users.
    pub fn set_transactions_rate(
        &mut self,
        rate_per_block: u64,
        users: Option<usize>,
    ) -> StepResult {
        if self.spec.transactions.is_some() {
            return Err(StepError::InvalidArgument {
                message: "transactions workload already configured".to_owned(),
            });
        }

        self.spec.transactions = Some(TransactionSpec {
            rate_per_block: non_zero!("transactions rate", rate_per_block)?,
            users: match users {
                Some(val) => Some(non_zero!("transactions users", val)?),
                None => None,
            },
        });
        Ok(())
    }

    /// Enable the consensus liveness expectation for this scenario.
    pub const fn enable_consensus_liveness(&mut self) {
        if self.spec.consensus_liveness.is_none() {
            self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
                lag_allowance: None,
            });
        }
    }

    /// Set the consensus liveness lag allowance in blocks. This configures how
    /// far behind the target height the nodes are allowed to be while still
    /// satisfying the expectation.
    pub fn set_consensus_liveness_lag_allowance(&mut self, blocks: u64) -> StepResult {
        self.spec.consensus_liveness = Some(ConsensusLivenessSpec {
            lag_allowance: Some(non_zero!("lag allowance", blocks)?),
        });

        Ok(())
    }

    /// Build a scenario for local deployment based on the current world
    /// configuration. This performs necessary preflight checks and returns
    /// a built scenario ready for deployment.
    pub fn build_local_scenario(&self) -> Result<Scenario<LbcEnv>, StepError> {
        let builder = self.make_builder_for_deployer(DeployerKind::Local)?;
        builder
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    /// Build a scenario for compose deployment based on the current world
    /// configuration. This performs necessary preflight checks and returns
    /// a built scenario ready for deployment.
    pub fn build_compose_scenario(
        &self,
    ) -> Result<Scenario<LbcEnv, NodeControlCapability>, StepError> {
        let builder = self.make_builder_for_deployer(DeployerKind::Compose)?;
        builder
            .enable_node_control()
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    /// Build a scenario for k8s deployment based on the current world
    /// configuration.
    pub fn build_k8s_scenario(&self) -> Result<Scenario<LbcEnv>, StepError> {
        let builder = self.make_builder_for_deployer(DeployerKind::K8s)?;
        builder
            .build()
            .map_err(|source| StepError::ScenarioBuild { source })
    }

    /// Perform preflight checks to ensure the world is properly configured for
    /// the expected deployer kind.
    pub fn preflight(&self, expected: DeployerKind) -> Result<(), StepError> {
        self.ensure_expected_deployer(expected)?;

        if expected.requires_local_node_binary() {
            Self::ensure_local_node_binary()?;
        }

        Ok(())
    }

    // Construct a scenario builder with the appropriate configuration for the
    // expected deployer kind. This checks that the deployer kind matches the
    // expected kind, and then applies the world configuration (topology,
    // duration, workloads, expectations) to the builder.
    fn make_builder_for_deployer(
        &self,
        expected: DeployerKind,
    ) -> Result<ScenarioBuilderWith, StepError> {
        self.ensure_expected_deployer(expected)?;

        let topology = self
            .spec
            .topology
            .clone()
            .ok_or(StepError::MissingTopology)?;
        let duration_secs = self
            .spec
            .duration_secs
            .ok_or(StepError::MissingRunDuration)?
            .get();

        let mut builder: ScenarioBuilderWith = make_builder(&topology);

        builder = builder.with_run_duration(Duration::from_secs(duration_secs));
        if let Some(wallets) = self.spec.wallets {
            builder = builder.initialize_wallet(wallets.total_funds, wallets.users.get());
        }

        if let Some(tx) = self.spec.transactions {
            builder = builder.transactions_with(|flow| {
                let mut flow = flow.rate(tx.rate_per_block.get());
                if let Some(users) = tx.users {
                    flow = flow.users(users.get());
                }
                flow
            });
        }

        if let Some(liveness) = self.spec.consensus_liveness {
            if let Some(lag) = liveness.lag_allowance {
                builder = builder
                    .with_expectation(ConsensusLiveness::default().with_lag_allowance(lag.get()));
            } else {
                builder = builder.expect_consensus_liveness();
            }
        }

        Ok(builder)
    }

    fn ensure_expected_deployer(&self, expected: DeployerKind) -> Result<(), StepError> {
        let actual = self.deployer.ok_or(StepError::MissingDeployer)?;

        if actual != expected {
            return Err(StepError::DeployerMismatch { expected, actual });
        }

        Ok(())
    }

    fn ensure_local_node_binary() -> Result<(), StepError> {
        if host_node_binary_from_env_var_available() {
            return Ok(());
        }

        if !running_in_ci() {
            return Ok(());
        }

        let default_binary = ci_node_binary_path().ok_or_else(missing_node_binary_error)?;
        warn_if_overriding_invalid_node_binary(&default_binary);
        let default_binary_display = default_binary.display().to_string();

        set_default_env(LOGOS_BLOCKCHAIN_NODE_BIN, &default_binary_display);

        Ok(())
    }

    /// Helper to resolve a node name to the actual started node name. This is
    /// useful for steps that refer to nodes by a logical name, and need to
    /// find the corresponding started node in the world.
    pub fn resolve_node_runtime_name(&self, node_name: &str) -> Result<String, StepError> {
        Ok(self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Runtime node '{node_name}' not found"),
            })?
            .started_node
            .name
            .clone())
    }

    /// Helper to resolve a wallet name to the actual node name that the wallet
    /// is associated with.
    pub fn resolve_wallet_node_name(&self, wallet_name: &str) -> Result<String, StepError> {
        Ok(self
            .wallet_info
            .get(wallet_name)
            .ok_or(StepError::LogicalError {
                message: format!("Wallet '{wallet_name}' not found"),
            })?
            .node_name
            .clone())
    }

    /// Helper to check if a node is configured for immediate start (not
    /// awaiting network readiness)
    pub fn network_immediate_start(&self, node_name: &str) -> bool {
        self.nodes_info
            .get(node_name)
            .is_some_and(|info| info.immediate_start)
    }

    /// Helper to resolve a list of node names to a `PeerSelection::Named` with
    /// their corresponding started node names.
    pub fn peer_selection_from_names(
        &self,
        initial_peers: &[String],
    ) -> Result<PeerSelection, StepError> {
        Ok(PeerSelection::Named(
            self.resolve_named_peers(initial_peers),
        ))
    }

    /// Helper to resolve a list of node names to their corresponding started
    /// node names.
    pub fn resolve_named_peers(&self, initial_peers: &[String]) -> Vec<String> {
        initial_peers
            .iter()
            .map(|peer| {
                self.resolve_node_runtime_name(peer)
                    .unwrap_or_else(|_| peer.clone())
            })
            .collect()
    }

    pub fn zone_node_http_client(&self) -> Result<NodeHttpClient, StepError> {
        let node_name = self.zone.node_name()?;
        self.resolve_node_http_client(node_name)
    }

    pub fn zone_node_url(&self) -> Result<Url, StepError> {
        Ok(self.zone_node_http_client()?.base_url().clone())
    }

    pub fn zone_node_http_client_for_sequencer(
        &self,
        sequencer_alias: &str,
    ) -> Result<NodeHttpClient, StepError> {
        let node_name = self.zone.sequencer_node_name(sequencer_alias)?;
        self.resolve_node_http_client(node_name)
    }

    pub fn zone_node_url_for_sequencer(&self, sequencer_alias: &str) -> Result<Url, StepError> {
        Ok(self
            .zone_node_http_client_for_sequencer(sequencer_alias)?
            .base_url()
            .clone())
    }

    /// Helper to resolve a node http client to the actual started node name.
    pub fn resolve_node_http_client(&self, node_name: &str) -> Result<NodeHttpClient, StepError> {
        Ok(self
            .nodes_info
            .get(node_name)
            .ok_or(StepError::LogicalError {
                message: format!("Node info for '{node_name}' not found in world"),
            })?
            .started_node
            .client
            .clone())
    }

    /// Helper to retrieve all node names.
    pub fn all_node_names(&self) -> Vec<String> {
        self.nodes_info.keys().cloned().collect::<Vec<_>>()
    }

    pub fn any_started_node(&self) -> Result<&NodeInfo, StepError> {
        self.nodes_info
            .values()
            .next()
            .ok_or_else(|| StepError::LogicalError {
                message: "No started nodes available in world".to_owned(),
            })
    }

    /// Helper to resolve all user wallet names to the actual wallet
    /// information.
    pub fn all_user_wallets(&self) -> Vec<WalletInfo> {
        self.wallet_info
            .values()
            .filter(|w| matches!(w.wallet_type, WalletType::User { .. }))
            .cloned()
            .collect::<Vec<_>>()
    }

    /// Helper to resolve all funding wallet names to the actual wallet
    /// information.
    pub fn all_funding_wallets(&self) -> Vec<WalletInfo> {
        self.wallet_info
            .values()
            .filter(|w| matches!(w.wallet_type, WalletType::Funding { .. }))
            .cloned()
            .collect::<Vec<_>>()
    }

    /// Helper to resolve a wallet name to the actual wallet information.
    pub fn resolve_wallet(&self, wallet_name: &str) -> Result<WalletInfo, StepError> {
        self.resolve_wallets(&[wallet_name.to_owned()])?
            .into_iter()
            .next()
            .ok_or(StepError::MissingWallet)
    }

    pub fn remember_submitted_transaction(&mut self, alias: String, tx_hash: TxHash) {
        self.submitted_transactions.insert(alias, tx_hash);
    }

    #[must_use]
    pub fn scanned_transaction_hashes_len(&self) -> usize {
        self.scanned_transaction_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn missing_scanned_transaction_hashes(&self, expected: &HashSet<TxHash>) -> Vec<TxHash> {
        let scanned_transaction_hashes = self
            .scanned_transaction_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        expected
            .iter()
            .copied()
            .filter(|hash| !scanned_transaction_hashes.contains(hash))
            .collect()
    }

    pub fn resolve_submitted_transaction(&self, alias: &str) -> Result<TxHash, StepError> {
        self.submitted_transactions
            .get(alias)
            .copied()
            .ok_or(StepError::LogicalError {
                message: format!("Transaction alias '{alias}' not found in world state"),
            })
    }

    pub fn remember_prepared_transaction(&mut self, alias: String, signed_tx: SignedMantleTx) {
        self.prepared_transactions.insert(alias, signed_tx);
    }

    pub fn resolve_prepared_transaction(&self, alias: &str) -> Result<SignedMantleTx, StepError> {
        self.prepared_transactions
            .get(alias)
            .cloned()
            .ok_or(StepError::LogicalError {
                message: format!("Prepared transaction alias '{alias}' not found in world state"),
            })
    }

    /// Helper to resolve multiple wallet names to their actual wallet
    /// information.
    pub fn resolve_wallets(&self, wallet_names: &[String]) -> Result<Vec<WalletInfo>, StepError> {
        wallet_names
            .iter()
            .map(|w| {
                self.wallet_info
                    .get(w)
                    .cloned()
                    .ok_or(StepError::LogicalError {
                        message: format!("Wallet '{w}' not found in world state"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()
    }

    /// Helper to submit a transaction to the node associated with the given
    /// wallet.
    pub async fn submit_transaction(
        &self,
        wallet: &WalletInfo,
        signed_tx: &SignedMantleTx,
        node_client: &NodeHttpClient,
    ) -> Result<(), StepError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            node_client.submit_transaction(signed_tx),
        )
        .await
        .map_err(|_| StepError::Timeout {
            message: format!(
                "Submit transaction '{}/{}' ",
                wallet.wallet_name, wallet.node_name
            ),
        })??;

        Ok(())
    }

    /// Helper to submit a funding wallet transaction to the node associated
    /// with the given wallet.
    pub async fn submit_funding_wallet_transaction(
        &self,
        wallet: &WalletInfo,
        body: WalletTransferFundsRequestBody,
    ) -> Result<TxHash, StepError> {
        let node = self
            .nodes_info
            .get(&wallet.node_name)
            .ok_or(StepError::LogicalError {
                message: format!(
                    "Node '{}' for wallet '{}' not found",
                    wallet.node_name, wallet.wallet_name
                ),
            })?;
        let response = tokio::time::timeout(
            Duration::from_secs(10),
            node.started_node.client.transfer_funds(body),
        )
        .await
        .map_err(|_| StepError::Timeout {
            message: format!(
                "Submit transaction '{}/{}' ",
                wallet.wallet_name, wallet.node_name
            ),
        })??;

        Ok(response.hash)
    }

    /// Helper to set the `deployment_config_override_path` in the world based
    /// on the `CUCUMBER_NODE_CONFIG_OVERRIDE` environment variable. This
    /// allows scenarios to specify a custom deployment config on disk that
    /// will be used when starting nodes, bypassing the generated
    /// genesis/test deployment.
    pub fn apply_deployment_config_override_path(&mut self) {
        self.deployment_config_override_path = env::var(CUCUMBER_NODE_CONFIG_OVERRIDE)
            .ok()
            .map(PathBuf::from);
    }

    /// Returns the same output as `full_debug_info`, but as an owned `String`.
    #[must_use]
    pub fn full_debug_info_string(&self) -> String {
        struct FullDebugInfo<'a>(&'a CucumberWorld);

        impl Debug for FullDebugInfo<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.full_debug_info(f)
            }
        }

        format!("{:?}", FullDebugInfo(self))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Debug output intentionally enumerates world state fields for diagnostics"
    )]
    pub fn full_debug_info(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wallet_diagnostics = self
            .wallet_diagnostics_for_debug()
            .unwrap_or_else(|_| empty_wallet_diagnostics());

        f.debug_struct("CucumberWorld")
            .field("deployer", &format!("{:?}", self.deployer))
            .field("scenario_base_dir", &self.scenario_base_dir)
            .field("spec", &format!("{:?}", self.spec))
            .field("run", &format!("{:?}", self.run))
            .field("membership_check", &self.membership_check)
            .field("readiness_checks", &self.readiness_checks)
            .field(
                "join_external_network",
                &format!("{:?}", self.join_external_network),
            )
            .field("zone", &self.zone.debug_summary())
            .field(
                "populate_ibd_peers",
                &format!("{:?}", self.populate_ibd_peers_from_initial_peers),
            )
            .field(
                "require_all_peers_mode_online_at_startup",
                &format!("{:?}", self.require_all_peers_mode_online_at_startup),
            )
            .field(
                "genesis_block_utxos",
                &format!("{:?}", self.genesis_block_utxos),
            )
            .field("local_cluster", {
                if self.local_cluster.is_some() {
                    &"Has LbcManualCluster"
                } else {
                    &"None"
                }
            })
            .field("k8s_manual_cluster", {
                if self.k8s_manual_cluster.is_some() {
                    &"Has LbcK8sManualCluster"
                } else {
                    &"None"
                }
            })
            .field("nodes_info", &nodes_info_display(&self.nodes_info))
            .field("genesis_tokens", &format!("{:?}", self.genesis_tokens))
            .field("wallet_info", &wallet_info_display(&self.wallet_info))
            .field("faucet_base_url", &format!("{:?}", self.faucet_base_url))
            .field("faucet_username", &format!("{:?}", self.faucet_username))
            .field("faucet_password", &format!("{:?}", self.faucet_password))
            .field(
                "faucet_task_handles",
                &format!("{}", self.faucet_task_handles.as_ref().map_or(0, Vec::len)),
            )
            .field("test_context", &format!("{:?}", self.test_context))
            .field(
                "wallet_accounts",
                &wallet_accounts_display(&self.wallet_accounts),
            )
            .field("scenario_fee_state", &fee_state_summary(&self.fee_state))
            .field(
                "scanned_transaction_hashes",
                &self.scanned_transaction_hashes_len(),
            )
            .field(
                "wallet_utxos_by_block",
                &wallet_utxos_by_block_display(&wallet_diagnostics),
            )
            .field(
                "wallet_pending_states",
                &wallet_pending_states_display(&wallet_diagnostics),
            )
            .field(
                "node_header_heights",
                &node_header_heights_display(&wallet_diagnostics),
            )
            .field("node_peer_ids", &node_peer_ids_display(&self.node_peer_ids))
            .field("node_groups", &self.node_groups)
            .field("node_to_group", &self.node_to_group)
            .field("blend_core_nodes", &format!("{:?}", self.blend_core_nodes))
            .field(
                "initial_override_peers_display",
                &initial_peers_override_display(self.initial_peers_override.as_ref()),
            )
            .field(
                "ibd_peers_override_display",
                &ibd_peers_override_display(self.ibd_peers_override.as_ref()),
            )
            .field(
                "public_cryptarchia_endpoint_peers",
                &public_cryptarchia_endpoint_peers_display(
                    self.public_cryptarchia_endpoint_peers.as_ref(),
                ),
            )
            .field(
                "user_config_overrides",
                &user_config_overrides_display(&self.user_config_overrides),
            )
            .field(
                "deployment_config_override_path",
                &deployment_config_override_path_display(
                    self.deployment_config_override_path.as_ref(),
                ),
            )
            .finish()
    }
}

fn host_node_binary_from_env_var_available() -> bool {
    env::var_os(LOGOS_BLOCKCHAIN_NODE_BIN)
        .map(PathBuf::from)
        .is_some_and(|path| path.is_file())
        || shared_host_bin_path("logos-blockchain-node").is_file()
}

fn ci_node_binary_path() -> Option<PathBuf> {
    let current_dir = env::current_dir().ok()?;
    let release_binary = current_dir.join(BIN_PATH_RELEASE);

    if matches!(std::fs::exists(&release_binary), Ok(true)) {
        return Some(release_binary);
    }

    None
}

fn warn_if_overriding_invalid_node_binary(path: &Path) {
    if env::var_os(LOGOS_BLOCKCHAIN_NODE_BIN).is_none() {
        return;
    }

    warn!(
        target: TARGET,
        "'{LOGOS_BLOCKCHAIN_NODE_BIN:?}' does not point to a valid file, overriding it to '{}'.",
        path.display()
    );
}

fn missing_node_binary_error() -> StepError {
    StepError::Preflight {
        message: format!(
            "Missing Logos host binary in CI. Set {LOGOS_BLOCKCHAIN_NODE_BIN}, \
            or build target/release/logos-blockchain-node before running Cucumber tests."
        ),
    }
}

fn running_in_ci() -> bool {
    env::var_os("CI").is_some() || env::var_os("GITHUB_ACTIONS").is_some()
}

fn wallet_block_feed_error(error: &CucumberWalletBlockFeedError) -> StepError {
    StepError::LogicalError {
        message: error.to_string(),
    }
}

fn wallet_state_lock_error() -> StepError {
    StepError::LogicalError {
        message: "wallet state lock is poisoned".to_owned(),
    }
}

fn wallet_feed_tracker_lock_error() -> StepError {
    StepError::LogicalError {
        message: "wallet feed tracker lock is poisoned".to_owned(),
    }
}

const fn empty_wallet_diagnostics() -> WalletDiagnostics {
    WalletDiagnostics {
        utxo_snapshot_count: 0,
        pending_wallet_count: 0,
        header_height_node_count: 0,
        pending_states: Vec::new(),
        utxo_snapshots: Vec::new(),
        header_heights: Vec::new(),
    }
}

fn nodes_info_display(nodes_info: &HashMap<String, NodeInfo>) -> String {
    let nodes: Vec<_> = nodes_info
        .iter()
        .map(|(k, v)| {
            let wallets: Vec<_> = v
                .wallet_info
                .values()
                .map(|w| w.wallet_name.clone())
                .collect();
            let wallets_str = format!("[{}]", wallets.join(", "));
            format!("'{k}: {} {wallets_str}'", v.started_node.name)
        })
        .collect();
    format!("HashMap<String, NodeInfo>({})", nodes.join(", "))
}

fn wallet_info_display(wallet_info: &WalletInfoMap) -> String {
    let wallets: Vec<_> = wallet_info
        .iter()
        .map(|(k, v)| format!("'{k}: {}'", v.wallet_name))
        .collect();
    format!("WalletInfoMap({})", wallets.join(", "))
}

fn wallet_accounts_display(wallet_accounts: &HashMap<usize, WalletAccount>) -> String {
    let accounts: Vec<_> = wallet_accounts
        .iter()
        .map(|(k, v)| format!("'{k}: {:?} {:?} {:?}'", v.label, v.value, v.secret_key))
        .collect();
    format!("HashMap<usize, WalletAccount>({})", accounts.join(", "))
}

fn wallet_utxos_by_block_display(wallet_diagnostics: &WalletDiagnostics) -> String {
    let blocks: Vec<_> = wallet_diagnostics
        .utxo_snapshots
        .iter()
        .filter_map(|snapshot| {
            if snapshot.non_empty_wallets.is_empty() {
                None
            } else {
                let non_empty_wallets: Vec<_> = snapshot
                    .non_empty_wallets
                    .iter()
                    .map(|(wallet, utxo_count)| format!("{wallet}: [{utxo_count}]"))
                    .collect();
                Some(format!(
                    "{}: {} {}",
                    snapshot.block_hash,
                    snapshot.header_id,
                    non_empty_wallets.join(" -")
                ))
            }
        })
        .collect();

    format!("HashMap<String, WalletUtxoSnapshot>({})", blocks.join(", "))
}

fn wallet_pending_states_display(wallet_diagnostics: &WalletDiagnostics) -> String {
    let states: Vec<_> = wallet_diagnostics
        .pending_states
        .iter()
        .map(|state| {
            format!(
                "'{}: encumbered={}, tracked_fees={}'",
                state.wallet_id, state.reserved_utxos, state.tracked_spent_fees
            )
        })
        .collect();

    format!("WalletPendingStates({})", states.join(", "))
}

fn fee_state_summary(fee_state: &ScenarioFeeState) -> String {
    let sponsor = fee_state.sponsored_genesis_account.map_or_else(
        || "none".to_owned(),
        |account| {
            format!(
                "{}x{}",
                account.token_count.get(),
                account.token_value.get()
            )
        },
    );

    format!(
        "sponsor={sponsor}, wallet_account={}, encumbered_by_wallet={}",
        fee_state.wallet_account.is_some(),
        fee_state.reserved_wallet_count(),
    )
}

fn node_header_heights_display(wallet_diagnostics: &WalletDiagnostics) -> String {
    let nodes: Vec<_> = wallet_diagnostics
        .header_heights
        .iter()
        .map(|(node_name, heights)| {
            format!(
                "{node_name}: [{}]",
                heights
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    format!(
        "HashMap<String, HashMap<String, u64>>({})",
        nodes.join(", ")
    )
}

fn node_peer_ids_display(node_peer_ids: &HashMap<String, PeerId>) -> String {
    let nodes: Vec<_> = node_peer_ids
        .iter()
        .map(|(k, v)| format!("'{k}: {v}'"))
        .collect();
    format!("HashMap<String, PeerId>({})", nodes.join(", "))
}

fn initial_peers_override_display(initial_peers_override: Option<&Vec<Multiaddr>>) -> String {
    initial_peers_override.as_ref().map_or_else(
        || "None".to_owned(),
        |peers| {
            let peers_str = peers
                .iter()
                .map(|p| format!("{p}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Some(Vec<Multiaddr>({peers_str}))")
        },
    )
}

fn ibd_peers_override_display(ibd_peers_override: Option<&HashSet<PeerId>>) -> String {
    ibd_peers_override.as_ref().map_or_else(
        || "None".to_owned(),
        |peers| {
            let peers_str = peers
                .iter()
                .map(|p| format!("{p}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Some(HashSet<PeerId>({peers_str}))")
        },
    )
}

fn blockchain_snapshot_on_startup_display(node_snapshot: Option<&NodeSnapshot>) -> String {
    node_snapshot.as_ref().map_or_else(
        || "None".to_owned(),
        |snapshot| format!("Some(NodeSnapshot({}-{}))", snapshot.name, snapshot.node),
    )
}

fn public_cryptarchia_endpoint_peers_display(
    public_cryptarchia_endpoint_peers: Option<&Vec<PublicCryptarchiaEndpointPeer>>,
) -> String {
    public_cryptarchia_endpoint_peers.as_ref().map_or_else(
        || "None".to_owned(),
        |&peers| {
            let peers_str = peers
                .iter()
                .map(|peer| format!("{} (user: {})", peer.url, peer.username))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Vec<PublicCryptarchiaEndpointPeer>({peers_str})")
        },
    )
}

fn deployment_config_override_path_display(
    deployment_config_override_path: Option<&PathBuf>,
) -> String {
    deployment_config_override_path.as_ref().map_or_else(
        || "None".to_owned(),
        |path| format!("Some({})", path.display()),
    )
}

fn user_config_overrides_display(overrides: &[ConfigOverride]) -> String {
    if overrides.is_empty() {
        return "[]".to_owned();
    }

    let values = overrides
        .iter()
        .map(|override_item| format!("{}={:?}", override_item.path, override_item.value))
        .collect::<Vec<_>>();
    format!("[{}]", values.join(", "))
}
