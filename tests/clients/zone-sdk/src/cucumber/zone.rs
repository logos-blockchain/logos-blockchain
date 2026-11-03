use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use lb_core::{
    mantle::{
        Value,
        ops::channel::{
            ChannelId, MsgId, deposit::DepositOp, inscribe::Inscription,
            withdraw::ChannelWithdrawOp,
        },
    },
    proofs::channel_multi_sig_proof::IndexedSignatures,
};
use lb_key_management_system_service::keys::{Ed25519Key, Ed25519PublicKey};
use logos_blockchain_tests::cucumber::error::StepError;
use reqwest::Url;
use tokio::task::JoinHandle;

use super::steps::zone::runner::{
    Event, IndexedSignature, InscriptionId, PreparedChannelConfig, SequencerCheckpoint,
    SequencerClient,
};

pub struct ZonePublishedMessage {
    pub payload: Inscription,
    pub inscription_id: Option<InscriptionId>,
    /// The channel message id, stable across a stale-refund rebuild.
    pub msg_id: Option<MsgId>,
}

pub type ZoneDiscardedPayloads = Arc<tokio::sync::Mutex<HashSet<Inscription>>>;
/// The first channel-view contract violation a sequencer's drive loop
/// recorded, shared with the checker that writes it.
pub type ZoneViewViolation = Arc<Mutex<Option<String>>>;

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
    /// One per run of the sequencer alias, so a violation survives a restart.
    view_violations: HashMap<String, Vec<ZoneViewViolation>>,
    default_sequencer_alias: Option<String>,
    published_messages: HashMap<String, ZonePublishedMessage>,
    submitted_deposits: HashMap<String, (DepositOp, Value)>,
    submitted_withdraws: HashMap<String, ChannelWithdrawOp>,
    account_balances: HashMap<String, i64>,
    published_order: Vec<String>,
    saved_checkpoints: HashMap<String, SequencerCheckpoint>,
    latest_checkpoints: HashMap<String, SequencerCheckpoint>,
    prepared_configs: HashMap<String, PreparedChannelConfig>,
    prepared_config_signatures: HashMap<String, IndexedSignatures>,
    sequencer_startups: HashMap<String, ZoneSequencerStartup>,
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

    /// A registered sequencer's public key, without touching its private key.
    pub fn sequencer_public_key(&self, alias: &str) -> Result<Ed25519PublicKey, StepError> {
        self.sequencer_signing_key(alias)
            .map(Ed25519Key::public_key)
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
        msg_id: Option<MsgId>,
        sequencer_alias: Option<&str>,
        checkpoint: Option<SequencerCheckpoint>,
    ) {
        self.published_order.push(alias.clone());
        self.published_messages.insert(
            alias,
            ZonePublishedMessage {
                payload,
                inscription_id,
                msg_id,
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

    pub fn published_message(&self, alias: &str) -> Result<&ZonePublishedMessage, StepError> {
        self.published_messages
            .get(alias)
            .ok_or_else(|| StepError::LogicalError {
                message: format!("Zone message alias '{alias}' is not tracked"),
            })
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

    pub fn remember_prepared_config(&mut self, alias: String, prepared: PreparedChannelConfig) {
        self.prepared_configs.insert(alias, prepared);
    }

    pub fn prepared_config(&self, alias: &str) -> Result<&PreparedChannelConfig, StepError> {
        self.prepared_configs
            .get(alias)
            .ok_or(StepError::LogicalError {
                message: format!("No prepared zone config transaction '{alias}'"),
            })
    }

    pub fn add_prepared_config_signature(
        &mut self,
        alias: String,
        IndexedSignature {
            channel_key_index,
            signature,
        }: IndexedSignature,
    ) {
        self.prepared_config_signatures
            .entry(alias)
            .or_default()
            .try_insert(channel_key_index, signature)
            .unwrap();
    }

    #[must_use]
    pub fn prepared_config_signatures(&self, alias: &str) -> IndexedSignatures {
        self.prepared_config_signatures
            .get(alias)
            .cloned()
            .unwrap_or_default()
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
        discarded_payloads: Option<ZoneDiscardedPayloads>,
        view_violation: ZoneViewViolation,
    ) {
        if let Some(runtime) = self.runtimes.remove(&alias) {
            runtime.abort_tasks();
        }
        self.view_violations
            .entry(alias.clone())
            .or_default()
            .push(view_violation);

        self.runtimes.insert(
            alias,
            ZoneSequencerRuntime {
                client: sequencer_client,
                task: sequencer_task,
                events: sequencer_events,
                checkpoint_rx,
                channel_view_rx,
                turn_to_write_rx,
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

    /// Channel-view contract violations recorded by any run of any sequencer,
    /// as `(alias, message)`.
    #[must_use]
    pub fn view_violations(&self) -> Vec<(String, String)> {
        let mut violations: Vec<(String, String)> = self
            .view_violations
            .iter()
            .flat_map(|(alias, runs)| {
                runs.iter().filter_map(|run| {
                    run.lock()
                        .ok()
                        .and_then(|violation| violation.clone())
                        .map(|message| (alias.clone(), message))
                })
            })
            .collect();
        violations.sort();
        violations
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
        self.submitted_withdraws.clear();
        self.account_balances.clear();
        self.published_order.clear();
        self.saved_checkpoints.clear();
        self.latest_checkpoints.clear();
        self.prepared_configs.clear();
        self.prepared_config_signatures.clear();
        self.expected_custom_payloads.clear();
        self.view_violations.clear();
    }

    pub fn remember_expected_custom_payloads(&mut self, payloads: Vec<Inscription>) {
        self.expected_custom_payloads = payloads;
    }

    #[must_use]
    pub fn expected_custom_payloads(&self) -> &[Inscription] {
        &self.expected_custom_payloads
    }
}
