//! The chain service and its operations, shared by all [`crate::phases`].

pub mod phases;

use core::fmt::{Debug, Display};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, StreamExt as _, future::join_all, stream};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockInfo};
use lb_core::{
    block::{Block, SignedHeader, UncleHeaders},
    events::Events,
    header::HeaderId,
    mantle::{
        OpRef, TxGasCalculator,
        ledger::verification_mode::StandardMode,
        traits::{MantleTx, PreverifiedMantleTransaction, SignedMantleTx},
        transactions::{GasPrices, states::Preverified},
    },
    sdp::ServiceType,
};
use lb_cryptarchia_engine::{Epoch, PrunedBlocks, Slot};
use lb_cryptarchia_sync::{BlocksUnavailableReason, ProviderResponse};
use lb_network_service::message::ChainSyncEvent;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use lb_utils::bounded::UpperBoundedVec;
use overwatch::{
    DynError,
    services::{relay::InboundRelay, state::StateUpdater},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{
    ChainServiceInfo, ConsensusMsg, Cryptarchia, CryptarchiaConsensusState, EpochStateQueryResult,
    Error, LOG_TARGET, LibUpdate, ProcessedBlockEvent, PrunedBlocksInfo, Query, metrics,
    notifier::ChainOnlineNotifier,
    relays::{BroadcastRelay, CryptarchiaConsensusRelays},
    storage::{StorageAdapter as _, adapters::StorageAdapter},
    sync::block_provider::BlockProvider,
};

pub struct ProcessBlockOutcome<Tx> {
    pub pruned_blocks: PrunedBlocks<HeaderId>,
    pub reorged_block_ids: Vec<HeaderId>,
    pub reorged_txs: Vec<Tx>,
}

// Source tips normally leave this map when they pass the LIB. These small
// limits also protect the diagnostic path during a long LIB stall.
const MAX_QUERY_SOURCES_PER_TIP: usize = 8;
const MAX_QUERY_SOURCE_TIPS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EpochStateQuerySource {
    requested_epoch: Epoch,
    requested_slot: Slot,
    source_tip_slot: Slot,
    source_lib_id: HeaderId,
    source_lib_slot: Slot,
}

#[derive(Default)]
struct EpochStateQuerySourceTracker {
    sources_by_tip: HashMap<HeaderId, Vec<EpochStateQuerySource>>,
}

impl EpochStateQuerySourceTracker {
    fn record(&mut self, source_tip_id: HeaderId, source: EpochStateQuerySource) {
        let sources = self.sources_by_tip.entry(source_tip_id).or_default();
        if !sources.contains(&source) {
            sources.push(source);
            if sources.len() > MAX_QUERY_SOURCES_PER_TIP {
                sources.remove(0);
            }
        }
        self.enforce_tip_limit();
    }

    fn take_for_tip(&mut self, tip_id: &HeaderId) -> Option<Vec<EpochStateQuerySource>> {
        self.sources_by_tip.remove(tip_id)
    }

    fn retire_behind_lib(&mut self, lib_slot: Slot) {
        self.sources_by_tip.retain(|_, sources| {
            sources.retain(|source| source.source_tip_slot > lib_slot);
            !sources.is_empty()
        });
    }

    fn enforce_tip_limit(&mut self) {
        while self.sources_by_tip.len() > MAX_QUERY_SOURCE_TIPS {
            let oldest_source_tip = self
                .sources_by_tip
                .iter()
                .min_by_key(|(_, sources)| {
                    sources.iter().map(|source| source.source_tip_slot).max()
                })
                .map(|(source_tip_id, _)| *source_tip_id);
            let Some(oldest_source_tip) = oldest_source_tip else {
                break;
            };
            self.sources_by_tip.remove(&oldest_source_tip);
        }
    }
}

/// The chain service in the phase `P`.
pub struct Service<Phase, Tx, Storage, RuntimeServiceId>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTransaction + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
{
    phase: Phase,
    cryptarchia: Cryptarchia,
    inbound_relay: InboundRelay<ConsensusMsg<Tx>>,
    state_updater: StateUpdater<Option<CryptarchiaConsensusState>>,
    new_block_subscription_sender: broadcast::Sender<ProcessedBlockEvent>,
    lib_subscription_sender: broadcast::Sender<LibUpdate>,
    chain_online_notifier: ChainOnlineNotifier,
    current_slot: Slot,
    storage_blocks_to_remove: HashSet<HeaderId>,
    relays: CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
    sync_blocks_provider: BlockProvider<Storage, Tx>,
    slot_timer: lb_time_service::EpochSlotTickStream,
    state_recording_timer: tokio::time::Interval,
    prolonged_bootstrap_period: Duration,
    epoch_state_query_sources: EpochStateQuerySourceTracker,
}

impl<Phase, Tx, Storage, RuntimeServiceId> Service<Phase, Tx, Storage, RuntimeServiceId>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    /// Move to the `NextPhase`, carrying all the shared ingredients over.
    fn with_phase<NextPhase: phases::Phase>(
        self,
        phase: NextPhase,
    ) -> Service<NextPhase, Tx, Storage, RuntimeServiceId> {
        Service {
            phase,
            cryptarchia: self.cryptarchia,
            inbound_relay: self.inbound_relay,
            state_updater: self.state_updater,
            new_block_subscription_sender: self.new_block_subscription_sender,
            lib_subscription_sender: self.lib_subscription_sender,
            chain_online_notifier: self.chain_online_notifier,
            current_slot: self.current_slot,
            storage_blocks_to_remove: self.storage_blocks_to_remove,
            relays: self.relays,
            sync_blocks_provider: self.sync_blocks_provider,
            slot_timer: self.slot_timer,
            state_recording_timer: self.state_recording_timer,
            prolonged_bootstrap_period: self.prolonged_bootstrap_period,
            epoch_state_query_sources: self.epoch_state_query_sources,
        }
    }

    /// Apply a block to the chain and reply with the result.
    async fn apply_block_and_reply(
        &mut self,
        block: Block<Tx>,
        reply_channel: oneshot::Sender<Result<(HeaderId, Vec<Tx>), Error>>,
    ) {
        match self.process_block_and_update_state(block).await {
            Ok(outcome) => {
                reply_channel
                    .send(Ok((self.cryptarchia.tip(), outcome.reorged_txs)))
                    .unwrap_or_else(|_| {
                        error!("Could not send process block result through channel");
                    });
            }
            Err(e) => {
                log_process_block_error(&e);
                reply_channel.send(Err(e)).unwrap_or_else(|_| {
                    error!("Could not send process block error through channel");
                });
            }
        }
    }

    /// Process a block and update the service state accordingly.
    ///
    /// On error, the service state is not mutated.
    async fn process_block_and_update_state(
        &mut self,
        block: Block<Tx>,
    ) -> Result<ProcessBlockOutcome<Tx>, Error> {
        let previous_lib_slot = self.cryptarchia.lib_branch().slot();
        let outcome = process_block(
            &mut self.cryptarchia,
            block,
            self.current_slot,
            &self.relays,
            &self.new_block_subscription_sender,
            &self.lib_subscription_sender,
        )
        .await?;

        self.log_epoch_state_query_sources_became_stale(
            outcome
                .reorged_block_ids
                .iter()
                .copied()
                .chain(outcome.pruned_blocks.stale_blocks().copied()),
        );
        if self.cryptarchia.lib_branch().slot() > previous_lib_slot {
            self.retire_epoch_state_query_sources_behind_lib();
        }

        self.storage_blocks_to_remove = delete_stale_blocks_from_storage(
            outcome.pruned_blocks.stale_blocks().copied(),
            &self.storage_blocks_to_remove,
            self.relays.storage_adapter(),
        )
        .await;

        self.record_recovery_state();

        Ok(outcome)
    }

    fn log_epoch_state_query_sources_became_stale(
        &mut self,
        stale_block_ids: impl IntoIterator<Item = HeaderId>,
    ) {
        let canonical_tip = self.cryptarchia.tip_branch();
        let mut reported_source_tips = HashSet::new();

        for stale_block_id in stale_block_ids {
            if !reported_source_tips.insert(stale_block_id) {
                continue;
            }
            let Some(query_sources) = self.epoch_state_query_sources.take_for_tip(&stale_block_id)
            else {
                continue;
            };

            for query_source in query_sources {
                warn!(
                    target: LOG_TARGET,
                    diagnostic = "blend_tsi_outage",
                    event = "epoch_state_query_source_became_stale",
                    requested_epoch = u32::from(query_source.requested_epoch),
                    requested_slot = u64::from(query_source.requested_slot),
                    source_tip_id = %stale_block_id,
                    source_lib_id = %query_source.source_lib_id,
                    source_lib_slot = u64::from(query_source.source_lib_slot),
                    stale_at_slot = u64::from(self.current_slot),
                    canonical_tip_id = %canonical_tip.id(),
                    "Epoch-state query source tip became non-canonical"
                );
            }
        }
    }

    fn retire_epoch_state_query_sources_behind_lib(&mut self) {
        let lib_slot = self.cryptarchia.lib_branch().slot();
        self.epoch_state_query_sources.retire_behind_lib(lib_slot);
    }

    /// Serve a read-only query. Available in every phase.
    #[expect(clippy::too_many_lines, reason = "TODO: refactor into funcs")]
    async fn process_query(&mut self, query: Query) {
        match query {
            Query::Info { reply_channel } => {
                reply_channel
                    .send(ChainServiceInfo {
                        cryptarchia_info: self.cryptarchia.info(),
                        phase: Phase::TAG,
                    })
                    .unwrap_or_else(|e| {
                        error!("Could not send consensus info through channel: {:?}", e);
                    });
            }
            Query::NewBlockSubscribe { sender } => {
                sender
                    .send(self.new_block_subscription_sender.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to new block channel");
                    });
            }
            Query::LibSubscribe { sender } => {
                sender
                    .send(self.lib_subscription_sender.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to LIB updates channel");
                    });
            }
            Query::GetHeaders {
                from_descendant,
                to_ancestor,
                reply_channel,
            } => {
                // default to tip block if not present
                let from_descendant = from_descendant.unwrap_or_else(|| self.cryptarchia.tip());
                // default to LIB block if not present
                let to_ancestor = to_ancestor.unwrap_or_else(|| self.cryptarchia.lib());

                let stream = get_block_ids(
                    &self.cryptarchia,
                    from_descendant,
                    to_ancestor,
                    self.relays.storage_adapter().clone(),
                );
                reply_channel
                    .send(stream)
                    .unwrap_or_else(|_| error!("could not send block stream through channel"));
            }
            Query::GetLedgerState {
                block_id,
                reply_channel,
            } => {
                let ledger_state = self.cryptarchia.ledger.state(&block_id).cloned();
                reply_channel.send(ledger_state).unwrap_or_else(|_| {
                    error!("Could not send ledger state through channel");
                });
            }
            Query::GetSdpDeclarations { reply_channel } => {
                let tip = self.cryptarchia.tip();
                let declarations = self
                    .cryptarchia
                    .ledger
                    .state(&tip)
                    .map(|ledger_state| ledger_state.mantle_ledger().sdp.declarations())
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|(_, declarations)| {
                        declarations
                            .iter()
                            .map(|(id, declaration)| (*id, declaration.clone()))
                    })
                    .collect();
                reply_channel.send(declarations).unwrap_or_else(|_| {
                    error!("Could not send SDP declarations through channel");
                });
            }
            Query::GetSdpSnapshot { reply_channel } => {
                let tip = self.cryptarchia.tip();
                let declarations = self
                    .cryptarchia
                    .ledger
                    .state(&tip)
                    .map(|ledger_state| {
                        ledger_state
                            .epoch_state()
                            .active_declarations
                            .iter()
                            .flat_map(|(_, declarations)| {
                                declarations
                                    .iter()
                                    .map(|(id, declaration)| (*id, declaration.clone()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                reply_channel.send(declarations).unwrap_or_else(|_| {
                    error!("Could not send SDP snapshot through channel");
                });
            }
            Query::GetEpochState {
                slot,
                reply_channel,
            } => {
                let result = self.cryptarchia.epoch_state_for_slot(slot);
                reply_channel.send(result).unwrap_or_else(|_| {
                    error!("Could not send epoch state through channel");
                });
            }
            Query::GetEpochStateWithSource {
                slot,
                reply_channel,
            } => {
                let result = self.cryptarchia.epoch_state_for_slot_with_source(slot);
                if let Ok(query_result) = &result {
                    self.track_epoch_state_query_source(query_result);
                    log_epoch_state_query(query_result);
                }
                reply_channel.send(result).unwrap_or_else(|_| {
                    error!("Could not send epoch state through channel");
                });
            }
            Query::GetEpochConfig { reply_channel } => {
                let config = self.cryptarchia.ledger.config();
                reply_channel
                    .send((config.epoch_config, config.consensus_config.clone()))
                    .unwrap_or_else(|_| {
                        error!("Could not send epoch config through channel");
                    });
            }
            Query::GetBlockEvents { id, reply_channel } => {
                let events = self.relays.storage_adapter().get_block_events(&id).await;
                reply_channel.send(events).unwrap_or_else(|_| {
                    error!("Could not send block events through channel");
                });
            }
            Query::SelectUncles {
                parent,
                slot,
                reply_channel,
            } => {
                let uncles = self.select_uncles(parent, slot).await;
                reply_channel.send(uncles).unwrap_or_else(|_| {
                    error!("Could not send uncles through channel");
                });
            }
            Query::SubscribeChainOnline { sender } => {
                sender
                    .send(self.chain_online_notifier.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to new block channel");
                    });
            }
        }
    }

    fn track_epoch_state_query_source(&mut self, query_result: &EpochStateQueryResult) {
        let query_source = EpochStateQuerySource {
            requested_epoch: query_result.requested_epoch,
            requested_slot: query_result.requested_slot,
            source_tip_slot: query_result.source_tip_slot,
            source_lib_id: query_result.source_lib_id,
            source_lib_slot: query_result.source_lib_slot,
        };
        self.epoch_state_query_sources
            .record(query_result.source_tip_id, query_source);
    }

    /// Selects uncles for a new block extending `parent` at `slot`.
    async fn select_uncles(&self, parent: HeaderId, slot: Slot) -> UncleHeaders {
        let Some(parent_branch) = self.cryptarchia.consensus.branches().get(&parent) else {
            return UncleHeaders::empty();
        };

        let mut uncles = Vec::new();
        for candidate in self
            .cryptarchia
            .consensus
            .select_uncles(parent_branch, slot)
        {
            // Every block accepted into the block tree is persisted, so a
            // candidate must be loadable. Even if not, a proposal is still
            // valid with fewer uncles.
            let Some(block) = self
                .relays
                .storage_adapter()
                .get_block(&candidate.id())
                .await
            else {
                error!(target: LOG_TARGET, candidate = ?candidate.id(), "uncle candidate not found in storage");
                continue;
            };
            uncles.push(SignedHeader::new(
                block.header().clone(),
                *block.signature(),
            ));
        }

        UncleHeaders::new(
            UpperBoundedVec::try_from(uncles).expect("at most MAX_UNCLES uncles are selected"),
        )
    }

    /// Record the current service state.
    fn record_recovery_state(&self) {
        persist_recovery_state(
            &self.cryptarchia,
            self.storage_blocks_to_remove.clone(),
            &self.state_updater,
        );
    }
}

fn log_epoch_state_query(result: &EpochStateQueryResult) {
    if !tracing::enabled!(target: LOG_TARGET, tracing::Level::DEBUG) {
        return;
    }
    let returned_active_declaration_count = result
        .epoch_state
        .active_declarations
        .iter()
        .map(|(_, declarations)| declarations.len())
        .sum::<usize>();

    debug!(
        target: LOG_TARGET,
        diagnostic = "blend_tsi_outage",
        event = "epoch_state_query",
        requested_slot = u64::from(result.requested_slot),
        requested_epoch = u32::from(result.requested_epoch),
        source_tip_id = %result.source_tip_id,
        source_tip_slot = u64::from(result.source_tip_slot),
        source_tip_height = result.source_tip_height,
        source_lib_id = %result.source_lib_id,
        source_lib_slot = u64::from(result.source_lib_slot),
        returned_epoch = u32::from(result.epoch_state.epoch),
        returned_nonce = ?result.epoch_state.nonce,
        returned_aged_utxo_root = ?result.epoch_state.utxo_merkle_root(),
        returned_lottery_0 = ?result.epoch_state.lottery_0,
        returned_lottery_1 = ?result.epoch_state.lottery_1,
        returned_blend_pow_difficulty = ?result.epoch_state.blend_pow_difficulty,
        returned_active_declaration_count,
        "Epoch state synthesized from chain tip"
    );
}

fn log_canonical_tsi_transition<Tx>(cryptarchia: &Cryptarchia, block: &Block<Tx>) {
    let parent_id = block.header().parent();
    let source_block_id = block.header().id();
    let (Some(parent_state), Some(committed_state)) = (
        cryptarchia.ledger.state(&parent_id),
        cryptarchia.ledger.state(&source_block_id),
    ) else {
        return;
    };
    let from_epoch = parent_state.epoch_state().epoch;
    let to_epoch = committed_state.epoch_state().epoch;
    if from_epoch == to_epoch {
        return;
    }

    let skipped_epochs = u32::from(to_epoch)
        .saturating_sub(u32::from(from_epoch))
        .saturating_sub(1);
    info!(
        target: LOG_TARGET,
        diagnostic = "blend_tsi_outage",
        event = "tsi_epoch_committed",
        canonical = true,
        from_epoch = u32::from(from_epoch),
        to_epoch = u32::from(to_epoch),
        skipped_epochs,
        source_block_slot = u64::from(block.header().slot()),
        source_block_id = %source_block_id,
        old_total_stake = parent_state.epoch_state().total_stake,
        new_total_stake = committed_state.epoch_state().total_stake,
        "Canonical TSI epoch state committed"
    );
}

fn log_canonical_sdp_activity<Tx>(cryptarchia: &Cryptarchia, parent_id: HeaderId, block: &Block<Tx>)
where
    Tx: MantleTx,
{
    let (Some(parent_state), Some(committed_state)) = (
        cryptarchia.ledger.state(&parent_id),
        cryptarchia.ledger.state(&block.header().id()),
    ) else {
        return;
    };

    for tx in block.transactions_iter() {
        for op in tx.op_refs_iter() {
            let OpRef::SDPActive(active) = op else {
                continue;
            };
            let Some(previous_declaration) = parent_state
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&active.declaration_id)
            else {
                continue;
            };
            let Some(new_declaration) = committed_state
                .mantle_ledger()
                .sdp_ledger()
                .get_declaration(&active.declaration_id)
            else {
                continue;
            };
            let inactivity_period = cryptarchia
                .ledger
                .config()
                .sdp_config
                .service_params
                .get(&new_declaration.service_type)
                .map_or(0, |params| {
                    params.inactivity_period.into_inner().into_inner()
                });

            info!(
                target: LOG_TARGET,
                diagnostic = "blend_tsi_outage",
                event = "sdp_activity_committed",
                canonical = true,
                provider_id = ?new_declaration.provider_id,
                declaration_id = ?active.declaration_id,
                proof_epoch = u32::from(active.metadata.origin_epoch()),
                tx_id = ?tx.hash(),
                block_id = %block.header().id(),
                block_slot = u64::from(block.header().slot()),
                epoch = u32::from(committed_state.epoch_state().epoch),
                previous_active_epoch = u32::from(previous_declaration.active),
                new_active_epoch = u32::from(new_declaration.active),
                active_until_epoch = u32::from(new_declaration.active).saturating_add(inactivity_period),
                "Canonical SDP activity committed"
            );
        }
    }
}

#[expect(
    clippy::cognitive_complexity,
    reason = "Snapshot diagnostics keep the frozen decision fields together."
)]
fn log_blend_snapshot_provider_decisions(
    target_epoch: Epoch,
    snapshot_slot: Slot,
    active_declarations: &lb_core::sdp::Declarations,
    source_declarations: &lb_core::sdp::Declarations,
    summary_event: &'static str,
    provider_event: &'static str,
) {
    let Some(all_blend_declarations) = source_declarations.for_service(&ServiceType::BlendNetwork)
    else {
        return;
    };
    let active_blend_declarations = active_declarations.for_service(&ServiceType::BlendNetwork);

    if tracing::enabled!(target: LOG_TARGET, tracing::Level::DEBUG) {
        let active_provider_ids: Vec<_> = active_blend_declarations
            .map(|active| {
                active
                    .values()
                    .map(|declaration| format!("{:?}", declaration.provider_id))
                    .collect()
            })
            .unwrap_or_default();
        debug!(
            target: LOG_TARGET,
            diagnostic = "blend_tsi_outage",
            event = summary_event,
            canonical = true,
            epoch = u32::from(target_epoch),
            snapshot_slot = u64::from(snapshot_slot),
            active_blend_declaration_count = active_blend_declarations.map_or(0, HashMap::len),
            active_provider_ids = ?active_provider_ids,
            "Canonical frozen active Blend declarations snapshot"
        );
    }

    for (declaration_id, declaration) in all_blend_declarations {
        let snapshot_declaration =
            active_blend_declarations.and_then(|active| active.get(declaration_id));
        match snapshot_declaration {
            Some(snapshot_declaration) => {
                debug!(
                    target: LOG_TARGET,
                    diagnostic = "blend_tsi_outage",
                    event = provider_event,
                    canonical = true,
                    target_epoch = u32::from(target_epoch),
                    snapshot_slot = u64::from(snapshot_slot),
                    provider_id = ?snapshot_declaration.provider_id,
                    declaration_id = ?declaration_id,
                    snapshot_active_epoch = u32::from(snapshot_declaration.active),
                    snapshot_withdraw_at = ?snapshot_declaration.withdraw_at.map(u32::from),
                    frozen_included = true,
                    "Canonical frozen Blend provider snapshot decision"
                );
            }
            None => {
                // The frozen EpochState retains only included declarations. The
                // historical active fields for an excluded provider are therefore
                // unavailable here and must not be reconstructed from the current
                // ledger declaration.
                debug!(
                    target: LOG_TARGET,
                    diagnostic = "blend_tsi_outage",
                    event = provider_event,
                    canonical = true,
                    target_epoch = u32::from(target_epoch),
                    snapshot_slot = u64::from(snapshot_slot),
                    provider_id = ?declaration.provider_id,
                    declaration_id = ?declaration_id,
                    frozen_included = false,
                    "Canonical frozen Blend provider snapshot decision"
                );
            }
        }
    }
}

fn log_canonical_blend_snapshots<Tx>(cryptarchia: &Cryptarchia, block: &Block<Tx>) {
    let parent_id = block.header().parent();
    let source_block_id = block.header().id();
    let (Some(parent_state), Some(committed_state)) = (
        cryptarchia.ledger.state(&parent_id),
        cryptarchia.ledger.state(&source_block_id),
    ) else {
        return;
    };

    if parent_id == cryptarchia.genesis_id {
        log_blend_snapshot_provider_decisions(
            Epoch::new(0),
            0.into(),
            &committed_state.epoch_state().active_declarations,
            &parent_state.mantle_ledger().sdp_ledger().declarations(),
            "blend_genesis_snapshot",
            "blend_genesis_snapshot_provider_decision",
        );
        log_blend_snapshot_provider_decisions(
            Epoch::new(1),
            0.into(),
            &committed_state.next_epoch_state().active_declarations,
            &parent_state.mantle_ledger().sdp_ledger().declarations(),
            "blend_genesis_snapshot",
            "blend_genesis_snapshot_provider_decision",
        );
    }

    let config = cryptarchia.ledger.config();
    for epoch_state in [
        committed_state.epoch_state(),
        committed_state.next_epoch_state(),
    ] {
        let target_epoch = epoch_state.epoch;
        if target_epoch <= Epoch::new(1) {
            continue;
        }
        let snapshot_slot = config.stake_distribution_snapshot(target_epoch);
        if parent_state.slot() < snapshot_slot && committed_state.slot() >= snapshot_slot {
            log_blend_snapshot_provider_decisions(
                target_epoch,
                snapshot_slot,
                &epoch_state.active_declarations,
                &parent_state.mantle_ledger().sdp_ledger().declarations(),
                "blend_canonical_snapshot",
                "blend_canonical_snapshot_provider_decision",
            );
        }
    }
}

/// Try to add a [`Block`] to [`Cryptarchia`].
///
/// A [`Block`] is only added if it's valid.
/// Otherwise, the [`Cryptarchia`] is unchanged and an error is returned.
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    level = "debug",
    skip(cryptarchia, block, relays, new_block_subscription_sender, lib_broadcaster),
    fields(block_id = %block.header().id(), tx_count = block.transactions_iter().count(), current_slot = ?current_slot)
)]
pub async fn process_block<Tx, Storage, RuntimeServiceId>(
    cryptarchia: &mut Cryptarchia,
    block: Block<Tx>,
    current_slot: Slot,
    relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
    new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
    lib_broadcaster: &broadcast::Sender<LibUpdate>,
) -> Result<ProcessBlockOutcome<Tx>, Error>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    debug!(target: LOG_TARGET, "Received proposal with ID: {:?}", block.header().id());
    let header = block.header().clone();
    let prev_lib = cryptarchia.lib();

    let mut candidate = cryptarchia.clone();
    let applied = candidate.try_apply_block_with_state_retention(block.clone(), current_slot)?;
    let new_lib = candidate.lib();

    let tx_count = block.transactions_iter().count();

    let immutable_blocks = immutable_blocks_index(
        &applied.pruned_blocks,
        Some(prev_lib),
        new_lib,
        candidate.consensus.lib_branch().slot(),
    );

    relays
        .storage_adapter()
        .store_block_data(
            header.id(),
            header.parent(),
            block.clone(),
            applied.events,
            immutable_blocks,
        )
        .await
        .map_err(|e| Error::Storage(format!("Failed to store block data: {e}")))?;

    log_newly_canonical_blocks(
        &candidate,
        &block,
        &applied.newly_canonical_blocks,
        relays.storage_adapter(),
    )
    .await;
    candidate.prune_ledger_states(applied.pruned_blocks.all());
    *cryptarchia = candidate;
    metrics::emit_block_transactions_metric(tx_count);

    let processed_block_event = {
        let tip = cryptarchia.tip_branch();
        let lib = cryptarchia.lib_branch();
        ProcessedBlockEvent {
            block_id: header.id(),
            block_slot: header.slot(),
            tip: tip.id(),
            tip_slot: tip.slot(),
            lib: lib.id(),
            lib_slot: lib.slot(),
        }
    };
    if let Err(e) = new_block_subscription_sender.send(processed_block_event) {
        debug!("No new-block subscribers to notify: {e}");
    }

    if prev_lib != new_lib {
        log_lib_advanced(
            &prev_lib,
            &new_lib,
            applied.pruned_blocks.stale_blocks().count(),
            applied.pruned_blocks.immutable_blocks().len(),
            applied.reorged_blocks.len(),
        );

        let height = cryptarchia
            .consensus
            .branches()
            .get(&cryptarchia.lib())
            .expect("LIB branch not available")
            .length();
        let block_info = BlockInfo {
            height,
            header_id: new_lib,
        };

        if let Err(e) = broadcast_finalized_block(relays.broadcast_relay(), block_info).await {
            warn!("Failed to notify finalized-block subscribers: {e}");
        }

        let lib_update = LibUpdate {
            new_lib: cryptarchia.lib(),
            pruned_blocks: PrunedBlocksInfo {
                stale_blocks: applied.pruned_blocks.stale_blocks().copied().collect(),
                immutable_blocks: applied.pruned_blocks.immutable_blocks().clone(),
            },
        };

        if let Err(e) = lib_broadcaster.send(lib_update) {
            warn!("No LIB-update subscribers to notify: {e}");
        }
    }

    let reorged_txs: Vec<_> = join_all(
        applied
            .reorged_blocks
            .iter()
            .map(|id| relays.storage_adapter().get_block(id)),
    )
    .await
    .into_iter()
    .flatten()
    .flat_map(Block::into_transactions)
    .collect();

    Ok(ProcessBlockOutcome {
        pruned_blocks: applied.pruned_blocks,
        reorged_block_ids: applied.reorged_blocks.iter().copied().collect(),
        reorged_txs,
    })
}

async fn log_newly_canonical_blocks<Tx, Storage, RuntimeServiceId>(
    cryptarchia: &Cryptarchia,
    applied_block: &Block<Tx>,
    newly_canonical_blocks: &[HeaderId],
    storage: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
) where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    if !tracing::enabled!(target: LOG_TARGET, tracing::Level::INFO) {
        return;
    }

    for block_id in newly_canonical_blocks {
        let canonical_block = if *block_id == applied_block.header().id() {
            Some(applied_block.clone())
        } else {
            storage.get_block(block_id).await
        };
        let Some(canonical_block) = canonical_block else {
            warn!(
                target: LOG_TARGET,
                diagnostic = "blend_tsi_outage",
                event = "canonical_diagnostic_block_unavailable",
                block_id = %block_id,
                "Could not load a newly canonical block for diagnostics"
            );
            continue;
        };
        log_canonical_sdp_activity(
            cryptarchia,
            canonical_block.header().parent(),
            &canonical_block,
        );
        log_canonical_blend_snapshots(cryptarchia, &canonical_block);
        log_canonical_tsi_transition(cryptarchia, &canonical_block);
    }
}

/// Returns block IDs from descendant (inclusive) to ancestor
/// (inclusive) in child-to-parent order.
///
/// First tries to find blocks from memory. If any block is missing from
/// memory, it falls back to loading all subsequent blocks from storage.
pub fn get_block_ids<Tx, Storage, RuntimeServiceId>(
    cryptarchia: &Cryptarchia,
    from_descendant: HeaderId,
    to_ancestor: HeaderId,
    storage_adapter: StorageAdapter<Storage, Tx, RuntimeServiceId>,
) -> Pin<Box<dyn Stream<Item = Result<HeaderId, Error>> + Send>>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    let branches = cryptarchia.consensus.branches();

    let mut in_memory = Vec::new();
    let mut current = from_descendant;
    while let Some(branch) = branches.get(&current) {
        in_memory.push(Ok(branch.id()));

        if branch.id() == to_ancestor {
            // All blocks are found in memory. Return immediately
            return Box::pin(stream::iter(in_memory));
        }
        if current == branch.parent() {
            debug!(target: LOG_TARGET, ?to_ancestor, "reached genesis while looking for ancestor from memory");
            // Return collected blocks and an error since we couldn't reach `to_ancestor`.
            return Box::pin(stream::iter(in_memory).chain(stream::once(async move {
                Err(Error::ParentIdNotFound(current))
            })));
        }

        current = branch.parent();
    }

    let storage_stream =
        stream::once(
            async move { load_block_ids_from_storage(current, to_ancestor, storage_adapter) },
        )
        .flatten();
    Box::pin(stream::iter(in_memory).chain(storage_stream))
}

/// Retrieves the block IDs from descendant (inclusive) to ancestor
/// (inclusive) from the storage, in child-to-parent order.
///
/// This is implemented here, and not as a method of `StorageAdapter`, to
/// simplify the panic and error message handling.
#[expect(closure_returning_async_block, reason = "required by try_unfold")]
pub fn load_block_ids_from_storage<Tx, Storage, RuntimeServiceId>(
    from_descendant: HeaderId,
    to_ancestor: HeaderId,
    storage: StorageAdapter<Storage, Tx, RuntimeServiceId>,
) -> impl Stream<Item = Result<HeaderId, Error>>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    // Yield `from_descendant` first since we already know it,
    // and yield subsequent parents by loading them from storage lazily.
    stream::once(async move { Ok(from_descendant) }).chain(stream::try_unfold(
            (from_descendant, storage),
            move |(current, storage)| async move {
                if current == to_ancestor {
                    // Reached `to_ancestor`. Terminate the stream
                    return Ok(None);
                }

                let parent = storage
                    .get_block_parent(&current)
                    .await
                    .ok_or(Error::ParentIdNotFound(current))?;

                if parent == current {
                    debug!(target: LOG_TARGET, ?to_ancestor, "reached genesis while looking for ancestor from storage");
                    // Terminate the stream with an error since we couldn't reach `to_ancestor`.
                    return Err(Error::ParentIdNotFound(current));
                }

                debug!(
                    target: LOG_TARGET, ?current, ?parent,
                    "loaded block parent from storage",
                );
                Ok(Some((parent, (parent, storage))))
            },
        ))
}

/// Remove the stale blocks from the storage layer.
///
/// Also, this removes the `additional_blocks` from the storage
/// layer. These blocks might belong to previous pruning operations and
/// that failed to be removed from the storage for some reason.
///
/// This function returns any block that fails to be deleted from the
/// storage layer.
pub async fn delete_stale_blocks_from_storage<Tx, Storage, RuntimeServiceId>(
    stale_blocks: impl Iterator<Item = HeaderId> + Send,
    additional_blocks: &HashSet<HeaderId>,
    storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
) -> HashSet<HeaderId>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    match delete_blocks_from_storage(
        stale_blocks.chain(additional_blocks.iter().copied()),
        storage_adapter,
    )
    .await
    {
        // No blocks failed to be deleted.
        Ok(()) => HashSet::new(),
        // We retain the blocks that failed to be deleted.
        Err(failed_blocks) => failed_blocks
            .into_iter()
            .map(|(block_id, _)| block_id)
            .collect(),
    }
}

/// Send a bulk blocks deletion request to the storage adapter.
///
/// If no request fails, the method returns `Ok()`.
/// If any request fails, the header ID and the generated error for each
/// failing request are collected and returned as part of the `Err`
/// result.
async fn delete_blocks_from_storage<Headers, Tx, Storage, RuntimeServiceId>(
    block_headers: Headers,
    storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
) -> Result<(), Vec<(HeaderId, DynError)>>
where
    Headers: Iterator<Item = HeaderId> + Send,
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + TxGasCalculator<Context = GasPrices>
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>> + Into<Bytes>,
    <Storage as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Display + 'static,
{
    let blocks_to_delete = block_headers.collect::<Vec<_>>();
    let block_deletion_outcomes = blocks_to_delete.iter().copied().zip(
        storage_adapter
            .remove_blocks(blocks_to_delete.iter().copied())
            .await,
    );

    let errors: Vec<_> = block_deletion_outcomes
        .filter_map(|(block_id, outcome)| match outcome {
            Ok(Some(_)) => {
                debug!(
                    target: LOG_TARGET,
                    "Block {block_id:#?} successfully deleted from storage."
                );
                None
            }
            Ok(None) => {
                trace!(
                    target: LOG_TARGET,
                    "Block {block_id:#?} was not found in storage."
                );
                None
            }
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    "Error deleting block {block_id:#?} from storage: {e}."
                );
                Some((block_id, e))
            }
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Builds the index of immutable block IDs, including the new LIB if needed.
/// If `prev_lib` is None, always includes the new LIB.
/// If `prev_lib` is Some, only includes new LIB if it changed.
fn immutable_blocks_index(
    pruned_blocks: &PrunedBlocks<HeaderId>,
    prev_lib: Option<HeaderId>,
    new_lib: HeaderId,
    new_lib_slot: Slot,
) -> BTreeMap<Slot, HeaderId> {
    let mut immutable_blocks = pruned_blocks.immutable_blocks().clone();
    // The new LIB is also immutable and should be immediately queryable by slot.
    // prune_immutable_blocks() only returns blocks older than the new LIB,
    // so we explicitly add the new LIB here.
    if prev_lib.is_none_or(|prev| prev != new_lib) {
        immutable_blocks.insert(new_lib_slot, new_lib);
    }

    immutable_blocks
}

async fn broadcast_finalized_block(
    broadcast_relay: &BroadcastRelay,
    block_info: BlockInfo,
) -> Result<(), DynError> {
    broadcast_relay
        .send(BlockBroadcastMsg::BroadcastFinalizedBlock(block_info))
        .await
        .map_err(|(error, _)| Box::new(error) as DynError)
}

/// Update and persist `CryptarchiaConsensusState`.
pub fn persist_recovery_state(
    cryptarchia: &Cryptarchia,
    storage_blocks_to_remove: HashSet<HeaderId>,
    state_updater: &StateUpdater<Option<CryptarchiaConsensusState>>,
) {
    match CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
        cryptarchia,
        storage_blocks_to_remove,
    ) {
        Ok(state) => {
            state_updater.update(Some(state));
        }
        Err(e) => {
            error!(target: LOG_TARGET, "Failed to update state: {}", e);
        }
    }
}

// TODO: use `send_chain_sync_rejection` for both, after checking callers
async fn reject_chain_sync_event(event: ChainSyncEvent) {
    debug!(target: LOG_TARGET, "rejecting chainsync event");
    match event {
        ChainSyncEvent::ProvideBlocksRequest { reply_sender, .. } => {
            let response = ProviderResponse::Unavailable {
                reason: BlocksUnavailableReason::Unknown("Node is not in online mode".to_owned()),
            };
            if let Err(err) = reply_sender.send(response).await {
                error!(target: LOG_TARGET, %err, "failed to send chain sync response");
            }
        }
        ChainSyncEvent::ProvideTipRequest { reply_sender } => {
            send_chain_sync_rejection(reply_sender).await;
        }
    }
}

async fn send_chain_sync_rejection<ResponseType>(
    sender: mpsc::Sender<ProviderResponse<ResponseType>>,
) {
    let response = ProviderResponse::Unavailable {
        reason: "Node is not in online mode".to_owned(),
    };
    if let Err(err) = sender.send(response).await {
        error!(target: LOG_TARGET, %err, "failed to send chain sync response");
    }
}

fn log_process_block_error(error: &Error) {
    let error_msg = format!("Failed to process block: {error:?}");
    if matches!(error, Error::FutureBlock { .. }) {
        trace!(target: LOG_TARGET, "{}", error_msg);
    } else {
        error!(target: LOG_TARGET, "{}", error_msg);
    }
}

fn log_lib_advanced(
    prev_lib: &HeaderId,
    new_lib: &HeaderId,
    stale_blocks_count: usize,
    immutable_blocks_count: usize,
    reorged_blocks_count: usize,
) {
    if stale_blocks_count == 0 && immutable_blocks_count == 1 && reorged_blocks_count == 0 {
        trace!(
            target: LOG_TARGET,
            "LIB advanced from {prev_lib:?} to {new_lib:?}; stale_blocks={stale_blocks_count}, immutable_blocks={immutable_blocks_count}, reorged_blocks={reorged_blocks_count}",
        );
    } else {
        debug!(
            target: LOG_TARGET,
            "LIB advanced from {prev_lib:?} to {new_lib:?}; stale_blocks={stale_blocks_count}, immutable_blocks={immutable_blocks_count}, reorged_blocks={reorged_blocks_count}",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_state_query_sources_are_bounded_and_retired_after_lib() {
        let source_tip_id = HeaderId::from([1; 32]);
        let source_lib_id = HeaderId::from([2; 32]);
        let mut tracker = EpochStateQuerySourceTracker::default();

        for requested_slot in 0..=MAX_QUERY_SOURCES_PER_TIP {
            tracker.record(
                source_tip_id,
                EpochStateQuerySource {
                    requested_epoch: Epoch::new(requested_slot as u32),
                    requested_slot: (requested_slot as u64).into(),
                    source_tip_slot: 10.into(),
                    source_lib_id,
                    source_lib_slot: 9.into(),
                },
            );
        }

        let sources = tracker
            .take_for_tip(&source_tip_id)
            .expect("source tip should be tracked");
        assert_eq!(sources.len(), MAX_QUERY_SOURCES_PER_TIP);
        assert_eq!(sources[0].requested_slot, 1.into());

        tracker.record(
            source_tip_id,
            EpochStateQuerySource {
                requested_epoch: Epoch::new(1),
                requested_slot: 1.into(),
                source_tip_slot: 10.into(),
                source_lib_id,
                source_lib_slot: 9.into(),
            },
        );
        tracker.retire_behind_lib(10.into());
        assert!(tracker.take_for_tip(&source_tip_id).is_none());
    }
}
