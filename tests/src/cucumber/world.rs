use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env,
    fmt::Debug,
    hash::BuildHasher,
    num::NonZero,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use cucumber::World;
use derivative::Derivative;
use lb_core::{
    codec::DeserializeOp as _,
    header::HeaderId,
    mantle::{
        GenesisTime, SignedMantleTx, Utxo, Value,
        ops::channel::{
            ChannelId, deposit::DepositOp, inscribe::Inscription, withdraw::ChannelWithdrawOp,
        },
        transactions::{
            hash::TxHash,
            states::{Preverified, VerificationState},
        },
    },
};
use lb_http_api_common::bodies::wallet::transfer_funds::WalletTransferFundsRequestBody;
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519PublicKey, ZkPublicKey};
use lb_libp2p::{Multiaddr, PeerId};
use lb_node::config::RunConfig;
use lb_testing_framework::{
    LbcEnv, LbcK8sManualCluster, LbcManualCluster, NodeHttpClient, ScenarioBuilder,
    ScenarioBuilderExt as _,
    configs::{deployment::SdpFundingConfig, wallet::WalletAccount},
    env::set_default_env,
    workloads,
};
use reqwest::Url;
use testing_framework_core::{
    scenario::{PeerSelection, Scenario, StartedNode},
    topology::DeploymentSeed,
};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{
    BIN_PATH_RELEASE,
    common::wallet::{
        TrackedWalletKeys, TrackedWalletKeysBySource, TrackedWallets, WalletDiagnostics,
        scanner::{
            ScannerSeed, SharedWalletScannerState, WalletScannerRuntime, WalletScannerState,
            build_fork_group_scanner_configs, start_wallet_scanners, wait_for_scanner_catch_up,
        },
    },
    cucumber::{
        TARGET,
        defaults::{
            CUCUMBER_NODE_CONFIG_OVERRIDE, LOGOS_BLOCKCHAIN_NODE_BIN, init_node_log_dir_defaults,
        },
        error::{StepError, StepResult},
        fee_reserve::{SCENARIO_FEE_ACCOUNT_NAME, ScenarioFeeState},
        steps::{
            tokio_console::profile::TokioConsoleProfile,
            zone::runner::{
                Event, InscriptionId, SequencerCheckpoint, SequencerClient, TxStatusUpdate,
            },
        },
        utils::{make_builder, shared_host_bin_path},
        wallet::snapshot::WalletSnapshot,
    },
    non_zero,
};

type ScenarioBuilderWith = ScenarioBuilder;
type ConsensusLiveness = workloads::ConsensusLiveness;
pub type SharedTrackedWallets = Arc<Mutex<TrackedWallets>>;
pub type SharedObservedTransactionHashes = Arc<Mutex<HashSet<TxHash>>>;

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
pub enum BlendDiagnosticPhase {
    Baseline,
    Outage,
    Recovery,
}

impl BlendDiagnosticPhase {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Outage => "outage",
            Self::Recovery => "recovery",
        }
    }
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
    client: SequencerClient,
    task: JoinHandle<()>,
    events: tokio::sync::broadcast::Receiver<Event>,
    checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
    channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
    turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
    tx_status_rx: Option<tokio::sync::broadcast::Receiver<TxStatusUpdate>>,
    discarded_payloads: Option<ZoneDiscardedPayloads>,
}

impl ZoneSequencerRuntime {
    fn abort_tasks(&self) {
        self.task.abort();
    }
}

#[derive(Clone, Copy, Default)]
pub struct ZoneSequencerStartup {
    pub pending_submit_depth: Option<usize>,
    pub passive_republish_orphans: bool,
}

/// Connection info for the read-only channel observer of the "zone indexer"
/// steps.
///
/// Each assertion cold-starts a fresh `ZoneSequencer` from this config with a
/// random signing key that is not part of the channel rotation — such a
/// sequencer can never publish or repost (inscription posting is turn-gated),
/// it only replays and observes finalized history.
#[derive(Clone)]
pub struct ZoneReaderConfig {
    pub channel_id: ChannelId,
    pub node_url: Url,
}

#[derive(Default)]
pub struct ZoneState {
    node_name: Option<String>,
    indexer: Option<ZoneReaderConfig>,
    sequencers: HashMap<String, ZoneSequencerIdentity>,
    runtimes: HashMap<String, ZoneSequencerRuntime>,
    default_sequencer_alias: Option<String>,
    published_messages: HashMap<String, ZonePublishedMessage>,
    submitted_deposits: HashMap<String, (DepositOp, Value)>,
    /// The channel notes each deposit re-created, keyed by deposit alias — so a
    /// later channel split transfer can spend them without waiting on the
    /// indexer.
    deposit_channel_notes: HashMap<String, Vec<Utxo>>,
    submitted_withdraws: HashMap<String, ChannelWithdrawOp>,
    account_balances: HashMap<String, i64>,
    published_order: Vec<String>,
    saved_checkpoints: HashMap<String, SequencerCheckpoint>,
    latest_checkpoints: HashMap<String, SequencerCheckpoint>,
    sequencer_startups: HashMap<String, ZoneSequencerStartup>,
    observed_mempool_pending: HashMap<String, HashSet<InscriptionId>>,
    sorted_total_payloads: Option<usize>,
    sorted_expected_by_sequencer: Option<HashMap<String, Vec<Inscription>>>,
    expected_custom_payloads: Vec<Inscription>,
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

    /// The signing key of whichever registered sequencer holds `public_key`,
    /// if any. Used to collect multi-sig channel-config signatures from the
    /// accredited key set a `PreparedChannelConfig` reports.
    #[must_use]
    pub fn sequencer_signing_key_for_public(
        &self,
        public_key: &Ed25519PublicKey,
    ) -> Option<&Ed25519Key> {
        self.sequencers
            .values()
            .find(|sequencer| sequencer.signing_key.public_key() == *public_key)
            .map(|sequencer| &sequencer.signing_key)
    }

    pub fn sequencer_channel_id(&self, alias: &str) -> Result<ChannelId, StepError> {
        self.sequencers
            .get(alias)
            .map(|sequencer| sequencer.channel_id)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not registered"),
            })
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

    pub fn remember_deposit_channel_notes(&mut self, alias: String, channel_notes: Vec<Utxo>) {
        self.deposit_channel_notes.insert(alias, channel_notes);
    }

    pub fn resolve_deposit_channel_notes(
        &self,
        alias: impl AsRef<str>,
    ) -> Result<&[Utxo], StepError> {
        let alias = alias.as_ref();

        self.deposit_channel_notes
            .get(alias)
            .map(Vec::as_slice)
            .ok_or(StepError::LogicalError {
                message: format!("Zone deposit channel notes for alias '{alias}' not found"),
            })
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

    pub fn message_tx_hashes_for_aliases(
        &self,
        aliases: &[String],
    ) -> Result<Vec<InscriptionId>, StepError> {
        aliases
            .iter()
            .map(|alias| {
                self.published_messages
                    .get(alias)
                    .and_then(|message| message.inscription_id)
                    .ok_or(StepError::LogicalError {
                        message: format!(
                            "Zone message alias '{alias}' does not have a tracked tx hash"
                        ),
                    })
            })
            .collect()
    }

    pub fn record_mempool_pending(
        &mut self,
        sequencer_alias: impl Into<String>,
        tx_hashes: impl IntoIterator<Item = InscriptionId>,
    ) {
        self.observed_mempool_pending
            .entry(sequencer_alias.into())
            .or_default()
            .extend(tx_hashes);
    }

    #[must_use]
    pub fn has_observed_mempool_pending(
        &self,
        sequencer_alias: &str,
        tx_hash: &InscriptionId,
    ) -> bool {
        self.observed_mempool_pending
            .get(sequencer_alias)
            .is_some_and(|observed| observed.contains(tx_hash))
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
        if let Some(checkpoint) = self
            .runtimes
            .get(sequencer_alias)
            .and_then(|runtime| runtime.checkpoint_rx.borrow().clone())
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
            .map(|runtime| runtime.checkpoint_rx.clone())
    }

    pub fn take_sequencer_tx_status_rx(
        &mut self,
        sequencer_alias: &str,
    ) -> Result<tokio::sync::broadcast::Receiver<TxStatusUpdate>, StepError> {
        self.runtimes
            .get_mut(sequencer_alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone sequencer '{sequencer_alias}' is not running"),
            })?
            .tx_status_rx
            .take()
            .ok_or_else(|| StepError::LogicalError {
                message: format!(
                    "Zone sequencer '{sequencer_alias}' tx-status receiver was already consumed"
                ),
            })
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
        sequencer_client: SequencerClient,
        sequencer_task: JoinHandle<()>,
        sequencer_events: tokio::sync::broadcast::Receiver<Event>,
        checkpoint_rx: tokio::sync::watch::Receiver<Option<SequencerCheckpoint>>,
        channel_view_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::SequencerChannelView>,
        turn_to_write_rx: tokio::sync::watch::Receiver<lb_zone_sdk::sequencer::TurnNotification>,
        tx_status_rx: tokio::sync::broadcast::Receiver<TxStatusUpdate>,
        discarded_payloads: Option<ZoneDiscardedPayloads>,
    ) {
        if let Some(runtime) = self.runtimes.remove(&alias) {
            runtime.abort_tasks();
        }

        self.runtimes.insert(
            alias,
            ZoneSequencerRuntime {
                client: sequencer_client,
                task: sequencer_task,
                events: sequencer_events,
                checkpoint_rx,
                channel_view_rx,
                turn_to_write_rx,
                tx_status_rx: Some(tx_status_rx),
                discarded_payloads,
            },
        );
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

        runtime.abort_tasks();

        Ok(())
    }

    pub fn sequencer_client(&self, alias: &str) -> Result<&SequencerClient, StepError> {
        self.runtimes
            .get(alias)
            .map(|runtime| &runtime.client)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
            })
    }

    pub fn sequencer_events_mut(
        &mut self,
        alias: &str,
    ) -> Result<&mut tokio::sync::broadcast::Receiver<Event>, StepError> {
        self.runtimes
            .get_mut(alias)
            .map(|runtime| &mut runtime.events)
            .ok_or(StepError::LogicalError {
                message: format!("Zone sequencer '{alias}' is not running"),
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

    pub fn set_indexer(&mut self, indexer: ZoneReaderConfig) {
        self.indexer = Some(indexer);
    }

    pub fn indexer(&self) -> Result<&ZoneReaderConfig, StepError> {
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
            runtime.abort_tasks();
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
        self.deposit_channel_notes.clear();
        self.submitted_withdraws.clear();
        self.account_balances.clear();
        self.published_order.clear();
        self.saved_checkpoints.clear();
        self.latest_checkpoints.clear();
        self.expected_custom_payloads.clear();
    }

    pub fn remember_expected_custom_payloads(&mut self, payloads: Vec<Inscription>) {
        self.expected_custom_payloads = payloads;
    }

    #[must_use]
    pub fn expected_custom_payloads(&self) -> &[Inscription] {
        &self.expected_custom_payloads
    }
}

#[derive(Debug, Clone)]
pub struct PublicCryptarchiaEndpointPeer {
    pub base_url: String,
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

/// This default slots per epoch value will be overwritten by scraping the
/// started node's deployment settings
const DEFAULT_SLOTS_PER_EPOCH: NonZero<u64> = NonZero::new(1).expect("one is non-zero");

/// Scenario identity and lifecycle configuration resolved by hooks and the
/// automated workload steps.
#[derive(Default)]
pub struct ScenarioLifecycle {
    /// The deployer kind that this scenario is configured for.
    pub deployer: Option<DeployerKind>,
    /// A unique per-scenario context string used to isolate runtime resources.
    pub test_context: Option<String>,
    /// Resolved genesis time for this Cucumber scenario attempt. It is set in
    /// the scenario hook and reused by every deployment build or rebuild.
    pub genesis_time: Option<GenesisTime>,
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
}

/// Chain and genesis parameters captured at cluster build time.
#[derive(Derivative)]
#[derivative(Default)]
pub struct ChainParameters {
    /// Manual: List of genesis block UTXOs allocated in the genesis
    /// configuration.
    pub genesis_block_utxos: Vec<Utxo>,
    /// Manual: Header id of the locally generated genesis block, when the
    /// cluster deployment carries one.
    pub genesis_block_id: Option<HeaderId>,
    /// Manual: List of genesis tokens allocated to wallets accounts.
    pub genesis_tokens: Vec<GenesisTokens>,
    /// Effective epoch length, populated from the first launched node's
    /// deployment config.
    #[derivative(Default(value = "DEFAULT_SLOTS_PER_EPOCH"))]
    pub slots_per_epoch: NonZero<u64>,
}

/// Manual-cluster deployment state: cluster instances, build recipe, and
/// node-level deployment facts.
#[derive(Default)]
pub struct ClusterState {
    /// Manual: Optional local cluster instance for scenarios that use the local
    /// deployer.
    pub local_cluster: Option<LbcManualCluster>,
    /// Manual: Optional k8s manual cluster instance for scenarios that use the
    /// k8s deployer.
    pub k8s_manual_cluster: Option<LbcK8sManualCluster>,
    /// Manual: Pending manual-cluster build recipe used to rebuild the local
    /// cluster when deployment-shape steps change before any nodes start.
    pub manual_cluster_spec: Option<ManualClusterSpec>,
    /// Manual: Stable deployment seed reused when the same scenario rebuilds a
    /// manual cluster, for example after restoring from a node snapshot.
    pub manual_cluster_deployment_seed: Option<DeploymentSeed>,
    /// Manual: Number of leading nodes declared as blend providers in the
    /// generated deployment. Defaults to all nodes when unset.
    pub blend_core_nodes: Option<usize>,
    /// Manual: Mapping of logical node names to their corresponding libp2p peer
    /// IDs.
    pub node_peer_ids: HashMap<String, PeerId>,
    /// Manual: SDP funding profile used by generated deployments.
    pub sdp_funding_config: SdpFundingConfig,
}

/// Runtime observations collected by the tagged Blend/TSI diagnostic
/// scenarios.
#[derive(Default)]
pub struct BlendDiagnosticState {
    /// Current phase of the diagnostic scenario.
    pub phase: Option<BlendDiagnosticPhase>,
    /// Number of epoch-observation steps completed by the scenario.
    pub observation_count: u32,
    /// Nodes successfully stopped during the diagnostic outage phase.
    pub stopped_nodes: HashSet<String>,
}

/// Node-startup configuration written by steps before nodes start and consumed
/// at node launch. Settings persist for the whole scenario so several node
/// starts and restarts can reuse them.
#[derive(Default)]
pub struct NodeStartupConfig {
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
    /// Manual: Whether to have dynamically started nodes join the external
    /// network
    pub join_external_network: Option<bool>,
    /// Manual: Runtime state for node-control extensions added outside the
    /// legacy generic step files.
    pub manual_node_config_overrides: ManualNodeConfigOverrides,
}

/// Snapshot work configured for the scenario: what to save when nodes stop,
/// what to restore before nodes start, and startup chain-state seeding.
#[derive(Debug, Default, Clone)]
pub struct SnapshotConfig {
    /// Manual: Snapshot work to perform when the scenario stops nodes.
    pub save: SnapshotSaveConfig,
    /// Manual: Snapshot work to perform before starting nodes from a snapshot.
    pub restore: SnapshotRestoreConfig,
    /// Manual: If set, dynamically started nodes should initialize their chain
    /// state from this named snapshot. This is a scenario-wide startup seeding
    /// setting.
    pub node_snapshot_on_startup: Option<NodeSnapshot>,
}

/// Transaction aliases tracked across steps: prepared and submitted
/// transactions plus their submission outcomes. All access goes through
/// `CucumberWorld` methods.
#[derive(Default)]
pub struct TransactionState {
    /// Manual: Mapping of scenario transaction aliases to submitted hashes.
    submitted_transactions: HashMap<String, TxHash>,
    /// Manual: Outcome of a transaction submission attempt, keyed by scenario
    /// alias, for scenarios that assert on submission being rejected rather
    /// than on later inclusion.
    submission_outcomes: HashMap<String, Result<(), String>>,
    /// Manual: Exact signed transactions prepared for later submission.
    prepared_transactions: HashMap<String, SignedMantleTx<Preverified>>,
    /// Manual: Initial fee arithmetic for percentage-funded transactions
    /// prepared by the fee-market steps.
    prepared_priority_fees: HashMap<String, PreparedPriorityFee>,
}

/// Wallet resources and funding state for the scenario.
#[derive(Default)]
pub struct WalletRegistry {
    /// Manual: Mapping of logical wallet names to their corresponding
    /// wallet resources.
    pub wallet_info: WalletInfoMap,
    /// Manual: Mapping of wallet account indices to their corresponding wallet
    /// account in the cluster.
    pub wallet_accounts: HashMap<usize, WalletAccount>,
    /// Manual: Public keys of wallet accounts whose secret keys are
    /// provisioned into node KMS configs at cluster build time.
    ///
    /// Node wallet services only index notes for keys they hold, so node-side
    /// wallet queries can be answered only for these keys. Accounts without
    /// genesis tokens are never provisioned and are absent here.
    pub node_provisioned_wallet_pks: HashSet<ZkPublicKey>,
    /// Manual: Scenario-level fee sponsor configuration and accounting.
    pub fee_state: ScenarioFeeState,
    /// Manual: Scenario-local wallet read model.
    ///
    /// This shared state contains wallet balances/UTXOs observed during a test
    /// scenario. Chain-derived UTXOs are updated exclusively by the
    /// background wallet scanner; step code must treat this state as
    /// read-only. Stored behind `Arc<Mutex<_>>` so scanner tasks and step
    /// code can safely share a single synchronized view for the scenario
    /// lifetime.
    pub wallets: SharedTrackedWallets,
    /// Manual: Faucet base URL configuration for manual transactions, if
    /// applicable.
    pub faucet_base_url: Option<String>,
    /// Manual: Task handles for dynamically spawned faucet funding tasks.
    pub faucet_task_handles: Option<Vec<JoinHandle<()>>>,
}

impl WalletRegistry {
    fn shutdown(&mut self) {
        if let Some(handles) = self.faucet_task_handles.take() {
            for handle in handles {
                handle.abort();
            }
        }
    }
}

/// Background wallet scanner ownership: shared diagnostics state, runtime task
/// handles, restore seeds, and chain observations.
#[derive(Default)]
pub struct WalletScanner {
    /// Manual: Background wallet scanner diagnostics state.
    pub state: SharedWalletScannerState,
    /// Manual: Background wallet scanner runtime.
    pub runtime: Option<WalletScannerRuntime>,
    /// Manual: Restored snapshot seeds available to wallet scanner startup,
    /// keyed by runtime node name.
    pub seeds: HashMap<String, ScannerSeed>,
    /// Manual: Transaction hashes observed in blocks by the wallet scanner.
    pub observed_transaction_hashes: SharedObservedTransactionHashes,
}

impl WalletScanner {
    fn shutdown(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.cancel();
        }
    }
}

/// Fork-group assignment of nodes: a forward map plus a reverse lookup kept in
/// lockstep. Empty means "no groups defined" and all nodes participate.
#[derive(Default)]
pub struct ForkGroups {
    /// `group_name` -> set of `node_names`.
    node_groups: HashMap<String, BTreeSet<String>>,
    /// `node_name` -> `group_name` reverse lookup.
    node_to_group: HashMap<String, String>,
}

impl ForkGroups {
    /// Replace all group assignments atomically, rejecting nodes assigned to
    /// more than one group.
    pub fn replace_all(
        &mut self,
        assignments: impl IntoIterator<Item = (String, String)>,
    ) -> Result<(), StepError> {
        let mut node_groups: HashMap<String, BTreeSet<String>> = HashMap::new();
        let mut node_to_group: HashMap<String, String> = HashMap::new();

        for (group_name, node_name) in assignments {
            if let Some(existing_group) = node_to_group.get(&node_name) {
                return Err(StepError::LogicalError {
                    message: format!(
                        "Node `{node_name}` appears in both group `{existing_group}` and `{group_name}`"
                    ),
                });
            }

            node_groups
                .entry(group_name.clone())
                .or_default()
                .insert(node_name.clone());
            node_to_group.insert(node_name, group_name);
        }

        self.node_groups = node_groups;
        self.node_to_group = node_to_group;

        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.node_groups.is_empty()
    }

    #[must_use]
    pub const fn groups(&self) -> &HashMap<String, BTreeSet<String>> {
        &self.node_groups
    }

    #[must_use]
    pub const fn mapping(&self) -> &HashMap<String, String> {
        &self.node_to_group
    }
}

#[derive(World, Default)]
pub struct CucumberWorld {
    /// Scenario identity and lifecycle configuration.
    pub lifecycle: ScenarioLifecycle,
    /// Chain and genesis parameters for the deployed cluster.
    pub chain: ChainParameters,
    /// Manual-cluster deployment state.
    pub cluster: ClusterState,
    /// Manual: List of nodes with their info.
    pub nodes_info: HashMap<String, NodeInfo>,
    /// Node-startup configuration overrides.
    pub startup: NodeStartupConfig,
    /// Snapshot save/restore configuration.
    pub snapshots: SnapshotConfig,
    /// Transaction aliases tracked across steps.
    txs: TransactionState,
    /// Wallet resources and funding state.
    pub wallet_registry: WalletRegistry,
    /// Background wallet scanner ownership.
    pub scanner: WalletScanner,
    /// Fork-group assignment of nodes.
    pub fork_groups: ForkGroups,
    /// Runtime observations for the tagged Blend/TSI diagnostics.
    pub blend_diagnostics: BlendDiagnosticState,
    /// Manual: Zone-specific state for SDK/sequencer scenarios.
    pub zone: ZoneState,
    /// Manual: Per-node Tokio console profiling requested by Cucumber steps.
    pub tokio_console_profile: TokioConsoleProfile,
    /// Manual: Per-block gas prices recorded by the fee-market steps,
    /// verified against the fee-market spec reference.
    pub recorded_gas_prices: Vec<crate::common::fee_spec::GasPriceRecord>,
    /// Manual: Wallet balances captured under a label, so a later step can
    /// assert a wallet's balance strictly increased relative to the recorded
    /// baseline (used by the `PoW` mining test to prove the reward landed).
    pub recorded_wallet_balances: HashMap<String, u64>,
}

impl Drop for CucumberWorld {
    fn drop(&mut self) {
        self.zone.clear();
        self.scanner.shutdown();
        self.wallet_registry.shutdown();
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

#[derive(Debug, Default, Clone)]
pub struct SnapshotSaveConfig {
    /// If set, all running node state is copied into this snapshot when nodes
    /// stop.
    pub node_state: Option<String>,
    /// If set, test-framework extension state is saved into this snapshot when
    /// nodes stop.
    pub extensions: Option<String>,
    /// Wallet extension payload prepared before node shutdown.
    pub prepared_wallet_snapshot: Option<WalletSnapshot>,
}

#[derive(Debug, Default, Clone)]
pub struct SnapshotRestoreConfig {
    /// If set, test-framework extension state is restored from this snapshot
    /// before nodes start.
    pub extensions: Option<String>,
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
            .field("deployer", &format!("{:?}", self.lifecycle.deployer))
            .field(
                "test_context",
                &format!("{:?}", self.lifecycle.test_context),
            )
            .field("genesis_time", &self.lifecycle.genesis_time)
            .field("scenario_base_dir", &self.lifecycle.scenario_base_dir)
            .field("spec", &format!("{:?}", self.lifecycle.spec))
            .field("run", &format!("{:?}", self.lifecycle.run))
            .field("membership_check", &self.lifecycle.membership_check)
            .field("readiness_checks", &self.lifecycle.readiness_checks)
            .field(
                "join_external_network",
                &format!("{:?}", self.startup.join_external_network),
            )
            .field(
                "populate_ibd_peers",
                &format!("{:?}", self.startup.populate_ibd_peers_from_initial_peers),
            )
            .field(
                "require_all_peers_mode_online_at_startup",
                &format!(
                    "{:?}",
                    self.startup.require_all_peers_mode_online_at_startup
                ),
            )
            .field(
                "genesis_block_utxos",
                &format!("{:?}", self.chain.genesis_block_utxos),
            )
            .field("genesis_block_id", &self.chain.genesis_block_id)
            .field("slots_per_epoch", &self.chain.slots_per_epoch)
            .field("local_cluster", {
                if self.cluster.local_cluster.is_some() {
                    &"Has LbcManualCluster"
                } else {
                    &"None"
                }
            })
            .field("k8s_manual_cluster", {
                if self.cluster.k8s_manual_cluster.is_some() {
                    &"Has LbcK8sManualCluster"
                } else {
                    &"None"
                }
            })
            .field("nodes_info", &self.nodes_info.len())
            .field("genesis_tokens", &self.chain.genesis_tokens.len())
            .field("wallet_info", &self.wallet_registry.wallet_info.len())
            .field(
                "faucet_base_url",
                &format!("{:?}", self.wallet_registry.faucet_base_url),
            )
            .field(
                "faucet_task_handles",
                &format!(
                    "{}",
                    self.wallet_registry
                        .faucet_task_handles
                        .as_ref()
                        .map_or(0, Vec::len)
                ),
            )
            .field(
                "wallet_accounts",
                &self.wallet_registry.wallet_accounts.len(),
            )
            .field(
                "node_provisioned_wallet_pks",
                &self.wallet_registry.node_provisioned_wallet_pks.len(),
            )
            .field(
                "scenario_fee_state",
                &fee_state_summary(&self.wallet_registry.fee_state),
            )
            .field("wallets", &"SharedTrackedWallets")
            .field(
                "submitted_transactions",
                &self.txs.submitted_transactions.len(),
            )
            .field(
                "recorded_wallet_balances",
                &self.recorded_wallet_balances.len(),
            )
            .field("submission_outcomes", &self.txs.submission_outcomes.len())
            .field(
                "prepared_transactions",
                &self.txs.prepared_transactions.len(),
            )
            .field(
                "prepared_priority_fees",
                &self.txs.prepared_priority_fees.len(),
            )
            .field(
                "observed_transaction_hashes",
                &self.observed_transaction_hashes_len(),
            )
            .field(
                "wallet_scanner_groups",
                &self
                    .scanner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .groups
                    .len(),
            )
            .field("wallet_scanner_runtime", &self.scanner.runtime.is_some())
            .field("wallet_scanner_seeds", &self.scanner.seeds.len())
            .field("wallet_utxos_by_block", &wallet_utxo_snapshot_count)
            .field("wallet_pending_states", &wallet_pending_count)
            .field(
                "scenario_fee_encumbered_tokens",
                &self.wallet_registry.fee_state.reserved_wallet_count(),
            )
            .field("node_header_heights", &wallet_header_height_node_count)
            .field("node_peer_ids", &self.cluster.node_peer_ids.len())
            .field("node_groups", &self.fork_groups.groups().len())
            .field("node_to_group", &self.fork_groups.mapping().len())
            .field("blend_core_nodes", &self.cluster.blend_core_nodes)
            .field("manual_cluster_spec", &self.cluster.manual_cluster_spec)
            .field(
                "manual_cluster_deployment_seed",
                &self.cluster.manual_cluster_deployment_seed.is_some(),
            )
            .field(
                "manual_node_config_overrides",
                &self.startup.manual_node_config_overrides,
            )
            .field("zone", &self.zone.debug_summary())
            .field(
                "initial_override_peers_display",
                &initial_peers_override_display(self.startup.initial_peers_override.as_ref()),
            )
            .field(
                "ibd_peers_override_display",
                &ibd_peers_override_display(self.startup.ibd_peers_override.as_ref()),
            )
            .field(
                "public_cryptarchia_endpoint_peers",
                &public_cryptarchia_endpoint_peers_display(
                    self.startup.public_cryptarchia_endpoint_peers.as_ref(),
                ),
            )
            .field(
                "user_config_overrides",
                &user_config_overrides_display(&self.startup.user_config_overrides),
            )
            .field(
                "deployment_config_overrides",
                &user_config_overrides_display(&self.startup.deployment_config_overrides),
            )
            .field("blend_diagnostic_phase", &self.blend_diagnostics.phase)
            .field(
                "blend_diagnostic_observation_count",
                &self.blend_diagnostics.observation_count,
            )
            .field(
                "blend_diagnostic_stopped_nodes",
                &self.blend_diagnostics.stopped_nodes,
            )
            .field("sdp_funding_config", &self.cluster.sdp_funding_config)
            .field(
                "deployment_config_override_path",
                &deployment_config_override_path_display(
                    self.startup.deployment_config_override_path.as_ref(),
                ),
            )
            .field("snapshot_save_config", &self.snapshots.save)
            .field("snapshot_restore_config", &self.snapshots.restore)
            .field(
                "node_snapshot_on_startup",
                &node_snapshot_on_startup_display(self.snapshots.node_snapshot_on_startup.as_ref()),
            )
            .field("tokio_console_profile", &self.tokio_console_profile)
            .field("recorded_gas_prices_len", &self.recorded_gas_prices.len())
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

/// Fee values captured when a percentage-funded transaction is prepared.
#[derive(Clone, Debug)]
pub struct PreparedPriorityFee {
    pub percent: u64,
    pub initial_mandatory_fee: u64,
    pub initial_reserve: u64,
    pub funded_fee: u64,
    pub initial_execution_price: u64,
    pub initial_storage_price: u64,
}

/// A scenario wallet is either user-owned or backed by a node wallet key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NodeWalletKeyRole {
    Funding,
    VoucherMaster,
    BlendZk,
    General,
}

impl NodeWalletKeyRole {
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::Funding => 0,
            Self::VoucherMaster => 1,
            Self::BlendZk => 2,
            Self::General => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NodeWalletKey {
    pub wallet_pk: String,
    pub role: NodeWalletKeyRole,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum WalletType {
    /// User-defined wallets are signed and tracked by the Cucumber harness.
    User { wallet_account: WalletAccount },
    /// Funding wallets are keys owned by a node and served by its wallet API.
    Funding { key: NodeWalletKey },
}

/// Information about a wallet resource created in the world, which can be used
/// to track and reference wallets across steps.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WalletInfo {
    /// Logical name of the wallet resource, used for referencing in steps.
    pub wallet_name: String,
    /// Logical name of the node resource where this wallet is referenced.
    pub node_name: String,
    /// The wallet type, which can be either a user-defined wallet or a funding
    /// wallet.
    pub wallet_type: WalletType,
}

/// A recipient accepted by transaction steps, with a display label for logs.
#[derive(Clone, Debug)]
pub struct WalletRecipient {
    pub label: String,
    pub public_key: ZkPublicKey,
}

impl WalletInfo {
    /// Helper to get the wallet's public key as `String` type (default hex).
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        match &self.wallet_type {
            WalletType::User { wallet_account, .. } => wallet_account.public_key_hex(),
            WalletType::Funding { key } => key.wallet_pk.clone(),
        }
    }

    /// Helper to get the wallet's public key as a `ZkPublicKey` type.
    pub fn public_key(&self) -> Result<ZkPublicKey, StepError> {
        match &self.wallet_type {
            WalletType::User { wallet_account, .. } => Ok(wallet_account.public_key()),
            WalletType::Funding { key } => {
                Ok(ZkPublicKey::from_bytes(&hex::decode(&key.wallet_pk)?)?)
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
    pub const fn is_node_wallet(&self) -> bool {
        matches!(self.wallet_type, WalletType::Funding { .. })
    }

    #[must_use]
    pub const fn is_node_funding_wallet(&self) -> bool {
        matches!(
            self.wallet_type,
            WalletType::Funding {
                key: NodeWalletKey {
                    role: NodeWalletKeyRole::Funding,
                    ..
                }
            }
        )
    }

    #[must_use]
    pub const fn is_scanner_tracked_wallet(&self) -> bool {
        self.is_user_wallet() || self.is_node_funding_wallet()
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
    /// Return the stable deployment seed for this manual-cluster scenario,
    /// generating it on first use.
    pub fn manual_cluster_deployment_seed(&mut self) -> DeploymentSeed {
        self.cluster
            .manual_cluster_deployment_seed
            .get_or_insert_with(|| DeploymentSeed::new(rand::random()))
            .clone()
    }

    /// Set a scenario-wide cryptarchia security parameter override for
    /// manual-cluster nodes.
    pub const fn set_cryptarchia_security_param(&mut self, security_param: NonZero<u32>) {
        self.startup
            .manual_node_config_overrides
            .set_cryptarchia_security_param(security_param);
    }

    /// Set a scenario-wide prolonged bootstrap period override for
    /// manual-cluster nodes.
    pub const fn set_prolonged_bootstrap_period(&mut self, period: Duration) {
        self.startup
            .manual_node_config_overrides
            .set_prolonged_bootstrap_period(period);
    }

    /// Set the SDP funding profile for generated manual-cluster deployments.
    pub const fn set_sdp_funding_config(&mut self, config: SdpFundingConfig) {
        self.cluster.sdp_funding_config = config;
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
        self.lifecycle.deployer = Some(deployer);
    }

    /// Set the directory where scenario artifacts should be stored.
    pub fn set_scenario_base_dir(&mut self, log_dir: &Path, deployer: &DeployerKind) {
        let log_dir = PathBuf::from(log_dir);
        init_node_log_dir_defaults(deployer, Some(&log_dir));

        self.lifecycle.scenario_base_dir.clone_from(&log_dir);
        if let Some(topology) = self.lifecycle.spec.topology.as_mut() {
            topology.scenario_base_dir = log_dir;
        }
    }

    pub fn set_test_context(&mut self, test_context: String) {
        self.lifecycle.test_context = Some(test_context);
    }

    pub const fn set_genesis_time(&mut self, genesis_time: GenesisTime) {
        self.lifecycle.genesis_time = Some(genesis_time);
    }

    /// Remove all scenario artifacts from the scenario base directory. This is
    /// useful for ensuring a clean state before starting a new scenario.
    pub fn clear_scenario_artifacts(&self) -> StepResult {
        if self.lifecycle.scenario_base_dir.is_dir() {
            std::fs::remove_dir_all(&self.lifecycle.scenario_base_dir).map_err(|e| {
                StepError::LogicalError {
                    message: format!(
                        "Failed to clear scenario artifacts in '{}': {e}",
                        self.lifecycle.scenario_base_dir.display()
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
        let wallets = self
            .wallet_registry
            .wallets
            .lock()
            .map_err(|_| wallet_state_lock_error())?;

        Ok(action(&wallets))
    }

    pub fn with_wallets_mut<R>(
        &self,
        action: impl FnOnce(&mut TrackedWallets) -> R,
    ) -> Result<R, StepError> {
        let mut wallets = self
            .wallet_registry
            .wallets
            .lock()
            .map_err(|_| wallet_state_lock_error())?;

        Ok(action(&mut wallets))
    }

    fn wallet_diagnostics_for_debug(&self) -> Result<WalletDiagnostics, StepError> {
        self.with_wallets(TrackedWallets::diagnostics)
    }

    pub async fn ensure_wallet_scanner_started(&mut self) -> StepResult {
        if self.scanner.runtime.is_some() {
            tokio::task::yield_now().await;
            return Ok(());
        }

        let scanner_state = Arc::new(Mutex::new(WalletScannerState::default()));
        let configs = build_fork_group_scanner_configs(self, Arc::clone(&scanner_state))?;
        let runtime = start_wallet_scanners(configs);

        self.scanner.state = scanner_state;
        self.scanner.runtime = Some(runtime);
        tokio::task::yield_now().await;
        Ok(())
    }

    pub async fn wait_for_wallet_scanner_catch_up(&mut self, timeout: Duration) -> StepResult {
        self.ensure_wallet_scanner_started().await?;
        wait_for_scanner_catch_up(&self.scanner.state, timeout).await
    }

    pub fn reset_wallet_scanner(&mut self) {
        if let Some(runtime) = self.scanner.runtime.take() {
            runtime.cancel();
        }
        self.scanner.state = Arc::new(Mutex::new(WalletScannerState::default()));
    }

    pub async fn reset_wallet_scanner_after_current_iteration(&mut self) {
        if let Some(runtime) = self.scanner.runtime.take() {
            runtime.shutdown_after_current_iteration().await;
        }
        self.scanner.state = Arc::new(Mutex::new(WalletScannerState::default()));
    }

    pub(crate) fn wallet_tracking_keys_for_source(
        &self,
        source_node_name: &str,
    ) -> Result<Vec<TrackedWalletKeys>, StepError> {
        let wallets_by_source = self.wallets_by_source_with_unique_public_keys()?;
        let Some(wallets) = wallets_by_source.get(source_node_name) else {
            return Ok(Vec::new());
        };

        let group_key = self
            .fork_groups
            .mapping()
            .get(source_node_name)
            .cloned()
            .unwrap_or_default();

        let mut wallet_keys = TrackedWalletKeysBySource::new();
        for (wallet_name, public_key) in wallets {
            wallet_keys.add_wallet(&group_key, wallet_name, *public_key);
        }

        if let Some(fee_wallet_account) = self.wallet_registry.fee_state.wallet_account.clone() {
            wallet_keys.add_wallet(
                &group_key,
                SCENARIO_FEE_ACCOUNT_NAME,
                fee_wallet_account.public_key(),
            );
        }

        Ok(wallet_keys
            .batches()
            .flat_map(|batch| batch.wallet_keys().to_vec())
            .collect())
    }

    fn wallets_by_source_with_unique_public_keys(
        &self,
    ) -> Result<HashMap<String, Vec<(String, ZkPublicKey)>>, StepError> {
        let mut wallets_by_source_and_key: HashMap<String, HashMap<ZkPublicKey, Vec<String>>> =
            HashMap::new();

        for wallet in self
            .wallet_registry
            .wallet_info
            .values()
            .filter(|wallet| wallet.is_scanner_tracked_wallet())
        {
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
                        "Skipping automatic scanner state tracking for aliases on `{}` with the same public key: {}",
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
        self.lifecycle.spec.topology = Some(TopologySpec {
            nodes: non_zero!("nodes", nodes)?,
            network,
            scenario_base_dir: self.lifecycle.scenario_base_dir.clone(),
        });
        Ok(())
    }

    /// Configure the scenario run duration in seconds.
    pub fn set_run_duration(&mut self, seconds: u64) -> StepResult {
        self.lifecycle.spec.duration_secs = Some(non_zero!("duration", seconds)?);
        Ok(())
    }

    // Configure the scenario wallets with total funds and number of users.
    pub fn set_wallets(&mut self, total_funds: u64, users: usize) -> StepResult {
        self.lifecycle.spec.wallets = Some(WalletSpec {
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
        if self.lifecycle.spec.transactions.is_some() {
            return Err(StepError::InvalidArgument {
                message: "transactions workload already configured".to_owned(),
            });
        }

        self.lifecycle.spec.transactions = Some(TransactionSpec {
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
        if self.lifecycle.spec.consensus_liveness.is_none() {
            self.lifecycle.spec.consensus_liveness = Some(ConsensusLivenessSpec {
                lag_allowance: None,
            });
        }
    }

    /// Set the consensus liveness lag allowance in blocks. This configures how
    /// far behind the target height the nodes are allowed to be while still
    /// satisfying the expectation.
    pub fn set_consensus_liveness_lag_allowance(&mut self, blocks: u64) -> StepResult {
        self.lifecycle.spec.consensus_liveness = Some(ConsensusLivenessSpec {
            lag_allowance: Some(non_zero!("lag allowance", blocks)?),
        });

        Ok(())
    }

    /// Check if Tokio console profiling is enabled for this scenario. This is
    /// determined by whether any profiling nodes have been configured in the
    /// `tokio_console_profile` field of the world.
    #[must_use]
    pub fn tokio_console_profile_enabled(&self) -> bool {
        !self.tokio_console_profile.profile_nodes.is_empty()
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
            .lifecycle
            .spec
            .topology
            .clone()
            .ok_or(StepError::MissingTopology)?;
        let duration_secs = self
            .lifecycle
            .spec
            .duration_secs
            .ok_or(StepError::MissingRunDuration)?
            .get();

        let mut builder: ScenarioBuilderWith = make_builder(&topology, self.lifecycle.genesis_time);

        builder = builder.with_run_duration(Duration::from_secs(duration_secs));
        if let Some(wallets) = self.lifecycle.spec.wallets {
            builder = builder.initialize_wallet(wallets.total_funds, wallets.users.get());
        }

        if let Some(tx) = self.lifecycle.spec.transactions {
            builder = builder.transactions_with(|flow| {
                let mut flow = flow.rate(tx.rate_per_block.get());
                if let Some(users) = tx.users {
                    flow = flow.users(users.get());
                }
                flow
            });
        }

        if let Some(liveness) = self.lifecycle.spec.consensus_liveness {
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
        let actual = self.lifecycle.deployer.ok_or(StepError::MissingDeployer)?;

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
            .wallet_registry
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn all_node_names(&self) -> Vec<String> {
        self.nodes_info.keys().cloned().collect::<Vec<_>>()
    }

    /// Helper to resolve all user wallet names to the actual wallet
    /// information.
    #[must_use]
    pub fn all_user_wallets(&self) -> Vec<WalletInfo> {
        self.wallet_registry
            .wallet_info
            .values()
            .filter(|w| matches!(w.wallet_type, WalletType::User { .. }))
            .cloned()
            .collect::<Vec<_>>()
    }

    /// Helper to resolve all node-owned wallet keys.
    #[must_use]
    pub fn all_node_wallets(&self) -> Vec<WalletInfo> {
        let mut wallets = self
            .wallet_registry
            .wallet_info
            .values()
            .filter(|wallet| wallet.is_node_wallet())
            .cloned()
            .collect::<Vec<_>>();
        wallets.sort_by(|left, right| left.wallet_name.cmp(&right.wallet_name));
        wallets
    }

    /// Resolve the node key configured to fund node service transactions.
    pub fn funding_wallet(&self, node_name: &str) -> Result<WalletInfo, StepError> {
        let mut wallets = self
            .wallet_registry
            .wallet_info
            .values()
            .filter(|wallet| wallet.node_name == node_name && wallet.is_node_funding_wallet())
            .cloned()
            .collect::<Vec<_>>();
        wallets.sort_by(|left, right| left.wallet_name.cmp(&right.wallet_name));
        match wallets.as_slice() {
            [wallet] => Ok(wallet.clone()),
            [] => Err(StepError::LogicalError {
                message: format!("Node `{node_name}` has no funding wallet"),
            }),
            _ => Err(StepError::LogicalError {
                message: format!("Node `{node_name}` has multiple funding wallets"),
            }),
        }
    }

    /// Resolve a scenario wallet name or a bare hexadecimal public key.
    pub fn resolve_recipient(&self, value: &str) -> Result<WalletRecipient, StepError> {
        if let Ok(wallet) = self.resolve_wallet(value) {
            let public_key = wallet.public_key()?;
            return Ok(WalletRecipient {
                label: wallet.wallet_name,
                public_key,
            });
        }

        let public_key = ZkPublicKey::from_bytes(&hex::decode(value).map_err(|_| {
            StepError::InvalidArgument {
                message: format!(
                    "Recipient `{value}` must be a scenario wallet name or bare hexadecimal public key"
                ),
            }
        })?)
        .map_err(|_| StepError::InvalidArgument {
            message: format!("Recipient `{value}` is not a valid public key"),
        })?;

        let mut matching_wallets = self
            .wallet_registry
            .wallet_info
            .values()
            .filter(|wallet| wallet.public_key().ok().as_ref() == Some(&public_key))
            .collect::<Vec<_>>();
        matching_wallets.sort_by(|left, right| {
            left.is_node_wallet()
                .cmp(&right.is_node_wallet())
                .then_with(|| left.wallet_name.cmp(&right.wallet_name))
        });

        Ok(WalletRecipient {
            label: matching_wallets
                .first()
                .map_or_else(|| value.to_owned(), |wallet| wallet.wallet_name.clone()),
            public_key,
        })
    }

    /// Helper to resolve a wallet name to the actual wallet information.
    pub fn resolve_wallet(&self, wallet_name: &str) -> Result<WalletInfo, StepError> {
        self.resolve_wallets(&[wallet_name.to_owned()])?
            .into_iter()
            .next()
            .ok_or(StepError::MissingWallet)
    }

    pub fn remember_submitted_transaction(&mut self, alias: String, tx_hash: TxHash) {
        self.txs.submitted_transactions.insert(alias, tx_hash);
    }

    /// All remembered submitted transactions whose alias starts with `prefix`,
    /// sorted by alias for deterministic reporting.
    #[must_use]
    pub fn submitted_transactions_with_prefix(&self, prefix: &str) -> Vec<(String, TxHash)> {
        let mut txs: Vec<_> = self
            .txs
            .submitted_transactions
            .iter()
            .filter(|(alias, _)| alias.starts_with(prefix))
            .map(|(alias, tx_hash)| (alias.clone(), *tx_hash))
            .collect();
        txs.sort();
        txs
    }

    pub fn remember_submission_outcome(&mut self, alias: String, outcome: Result<(), String>) {
        self.txs.submission_outcomes.insert(alias, outcome);
    }

    pub fn resolve_submission_outcome(
        &self,
        alias: &str,
    ) -> Result<&Result<(), String>, StepError> {
        self.txs
            .submission_outcomes
            .get(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Submission outcome for alias '{alias}' not found in world state"),
            })
    }

    #[must_use]
    pub fn observed_transaction_hashes_len(&self) -> usize {
        self.scanner
            .observed_transaction_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn missing_observed_transaction_hashes<S: BuildHasher>(
        &self,
        expected: &HashSet<TxHash, S>,
    ) -> Vec<TxHash> {
        let observed_transaction_hashes = self
            .scanner
            .observed_transaction_hashes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        expected
            .iter()
            .copied()
            .filter(|hash| !observed_transaction_hashes.contains(hash))
            .collect()
    }

    pub fn resolve_submitted_transaction(&self, alias: &str) -> Result<TxHash, StepError> {
        self.txs
            .submitted_transactions
            .get(alias)
            .copied()
            .ok_or(StepError::LogicalError {
                message: format!("Transaction alias '{alias}' not found in world state"),
            })
    }

    pub fn remember_prepared_transaction(
        &mut self,
        alias: String,
        signed_tx: SignedMantleTx<Preverified>,
    ) {
        self.txs.prepared_transactions.insert(alias, signed_tx);
    }

    pub fn remember_prepared_priority_fee(&mut self, alias: String, fee: PreparedPriorityFee) {
        self.txs.prepared_priority_fees.insert(alias, fee);
    }

    pub fn resolve_prepared_priority_fee(
        &self,
        alias: &str,
    ) -> Result<&PreparedPriorityFee, StepError> {
        self.txs
            .prepared_priority_fees
            .get(alias)
            .ok_or(StepError::LogicalError {
                message: format!("Prepared priority fee alias '{alias}' not found in world state"),
            })
    }

    pub fn resolve_prepared_transaction(
        &self,
        alias: &str,
    ) -> Result<SignedMantleTx<Preverified>, StepError> {
        self.txs
            .prepared_transactions
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
                self.wallet_registry
                    .wallet_info
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
    pub async fn submit_transaction<State>(
        &self,
        wallet: &WalletInfo,
        signed_tx: &SignedMantleTx<State>,
        node_client: &NodeHttpClient,
    ) -> Result<(), StepError>
    where
        State: VerificationState + Clone + Send + Sync + 'static,
    {
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
        self.startup.deployment_config_override_path = env::var(CUCUMBER_NODE_CONFIG_OVERRIDE)
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
        reason = "Flat field-by-field dump of the whole world state; splitting it would not \
                  improve readability"
    )]
    pub fn full_debug_info(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wallet_diagnostics = self
            .wallet_diagnostics_for_debug()
            .unwrap_or_else(|_| empty_wallet_diagnostics());

        f.debug_struct("CucumberWorld")
            .field("deployer", &format!("{:?}", self.lifecycle.deployer))
            .field("scenario_base_dir", &self.lifecycle.scenario_base_dir)
            .field("spec", &format!("{:?}", self.lifecycle.spec))
            .field("run", &format!("{:?}", self.lifecycle.run))
            .field("membership_check", &self.lifecycle.membership_check)
            .field("readiness_checks", &self.lifecycle.readiness_checks)
            .field(
                "join_external_network",
                &format!("{:?}", self.startup.join_external_network),
            )
            .field("zone", &self.zone.debug_summary())
            .field(
                "populate_ibd_peers",
                &format!("{:?}", self.startup.populate_ibd_peers_from_initial_peers),
            )
            .field(
                "require_all_peers_mode_online_at_startup",
                &format!(
                    "{:?}",
                    self.startup.require_all_peers_mode_online_at_startup
                ),
            )
            .field(
                "genesis_block_utxos",
                &format!("{:?}", self.chain.genesis_block_utxos),
            )
            .field("genesis_block_id", &self.chain.genesis_block_id)
            .field("local_cluster", {
                if self.cluster.local_cluster.is_some() {
                    &"Has LbcManualCluster"
                } else {
                    &"None"
                }
            })
            .field("k8s_manual_cluster", {
                if self.cluster.k8s_manual_cluster.is_some() {
                    &"Has LbcK8sManualCluster"
                } else {
                    &"None"
                }
            })
            .field("nodes_info", &nodes_info_display(&self.nodes_info))
            .field(
                "genesis_tokens",
                &format!("{:?}", self.chain.genesis_tokens),
            )
            .field(
                "wallet_info",
                &wallet_info_display(&self.wallet_registry.wallet_info),
            )
            .field(
                "faucet_base_url",
                &format!("{:?}", self.wallet_registry.faucet_base_url),
            )
            .field(
                "faucet_task_handles",
                &format!(
                    "{}",
                    self.wallet_registry
                        .faucet_task_handles
                        .as_ref()
                        .map_or(0, Vec::len)
                ),
            )
            .field(
                "test_context",
                &format!("{:?}", self.lifecycle.test_context),
            )
            .field(
                "wallet_accounts",
                &wallet_accounts_display(&self.wallet_registry.wallet_accounts),
            )
            .field(
                "scenario_fee_state",
                &fee_state_summary(&self.wallet_registry.fee_state),
            )
            .field(
                "observed_transaction_hashes",
                &self.observed_transaction_hashes_len(),
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
            .field(
                "node_peer_ids",
                &node_peer_ids_display(&self.cluster.node_peer_ids),
            )
            .field("node_groups", &self.fork_groups.groups())
            .field("node_to_group", &self.fork_groups.mapping())
            .field(
                "blend_core_nodes",
                &format!("{:?}", self.cluster.blend_core_nodes),
            )
            .field(
                "initial_override_peers_display",
                &initial_peers_override_display(self.startup.initial_peers_override.as_ref()),
            )
            .field(
                "ibd_peers_override_display",
                &ibd_peers_override_display(self.startup.ibd_peers_override.as_ref()),
            )
            .field(
                "public_cryptarchia_endpoint_peers",
                &public_cryptarchia_endpoint_peers_display(
                    self.startup.public_cryptarchia_endpoint_peers.as_ref(),
                ),
            )
            .field(
                "user_config_overrides",
                &user_config_overrides_display(&self.startup.user_config_overrides),
            )
            .field(
                "deployment_config_override_path",
                &deployment_config_override_path_display(
                    self.startup.deployment_config_override_path.as_ref(),
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

fn wallet_state_lock_error() -> StepError {
    StepError::LogicalError {
        message: "wallet state lock is poisoned".to_owned(),
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

fn node_snapshot_on_startup_display(node_snapshot: Option<&NodeSnapshot>) -> String {
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
                .map(|peer| format!("{} (user: {})", peer.base_url, peer.username))
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

#[cfg(test)]
mod node_wallet_tests {
    use super::{NodeWalletKey, NodeWalletKeyRole, WalletInfo, WalletType};

    fn node_wallet(role: NodeWalletKeyRole) -> WalletInfo {
        WalletInfo {
            wallet_name: "NODE_1_WALLET".to_owned(),
            node_name: "NODE_1".to_owned(),
            wallet_type: WalletType::Funding {
                key: NodeWalletKey {
                    wallet_pk: "00".repeat(32),
                    role,
                },
            },
        }
    }

    #[test]
    fn scanner_tracks_only_the_node_funding_role() {
        assert!(node_wallet(NodeWalletKeyRole::Funding).is_scanner_tracked_wallet());
        assert!(!node_wallet(NodeWalletKeyRole::VoucherMaster).is_scanner_tracked_wallet());
        assert!(!node_wallet(NodeWalletKeyRole::BlendZk).is_scanner_tracked_wallet());
        assert!(!node_wallet(NodeWalletKeyRole::General).is_scanner_tracked_wallet());
    }
}

#[cfg(test)]
mod fork_groups_tests {
    use super::ForkGroups;

    fn assignments(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(group, node)| ((*group).to_owned(), (*node).to_owned()))
            .collect()
    }

    #[test]
    fn replace_all_populates_both_maps() {
        let mut fork_groups = ForkGroups::default();

        fork_groups
            .replace_all(assignments(&[
                ("A", "NODE_1"),
                ("A", "NODE_2"),
                ("B", "NODE_3"),
            ]))
            .expect("valid assignments are accepted");

        assert!(!fork_groups.is_empty());
        assert_eq!(fork_groups.groups().len(), 2);
        assert!(fork_groups.groups()["A"].contains("NODE_1"));
        assert!(fork_groups.groups()["A"].contains("NODE_2"));
        assert!(fork_groups.groups()["B"].contains("NODE_3"));
        assert_eq!(fork_groups.mapping()["NODE_1"], "A");
        assert_eq!(fork_groups.mapping()["NODE_2"], "A");
        assert_eq!(fork_groups.mapping()["NODE_3"], "B");
    }

    #[test]
    fn replace_all_replaces_previous_assignments() {
        let mut fork_groups = ForkGroups::default();

        fork_groups
            .replace_all(assignments(&[("A", "NODE_1")]))
            .expect("valid assignments are accepted");
        fork_groups
            .replace_all(assignments(&[("B", "NODE_2")]))
            .expect("valid assignments are accepted");

        assert!(!fork_groups.groups().contains_key("A"));
        assert!(!fork_groups.mapping().contains_key("NODE_1"));
        assert_eq!(fork_groups.mapping()["NODE_2"], "B");
    }

    #[test]
    fn replace_all_rejects_node_assigned_to_two_groups() {
        let mut fork_groups = ForkGroups::default();

        let error = fork_groups
            .replace_all(assignments(&[("A", "NODE_1"), ("B", "NODE_1")]))
            .expect_err("duplicate node assignment is rejected");

        assert!(error.to_string().contains("NODE_1"));
    }

    #[test]
    fn rejected_replace_all_preserves_previous_state() {
        let mut fork_groups = ForkGroups::default();

        fork_groups
            .replace_all(assignments(&[("A", "NODE_1")]))
            .expect("valid assignments are accepted");
        fork_groups
            .replace_all(assignments(&[("B", "NODE_2"), ("C", "NODE_2")]))
            .expect_err("duplicate node assignment is rejected");

        assert_eq!(fork_groups.groups().len(), 1);
        assert!(fork_groups.groups()["A"].contains("NODE_1"));
        assert_eq!(fork_groups.mapping()["NODE_1"], "A");
        assert!(!fork_groups.mapping().contains_key("NODE_2"));
    }
}
