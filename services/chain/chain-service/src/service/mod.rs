//! The chain service and its operations, shared by all [`crate::phases`].

pub mod phases;

use core::fmt::Debug;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures::{
    Stream, StreamExt as _,
    future::join_all,
    stream::{self, FuturesUnordered},
};
use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockInfo};
use lb_core::{
    block::{Block, SignedHeader, TxInBlock, UncleHeaders},
    era::ForkDigest,
    header::HeaderId,
    mantle::{
        OpRef, TxHash,
        ledger::verification_mode::StandardMode,
        traits::{Hashable, MantleTx, PreverifiedMantleTransaction, SignedMantleTx, StorageSize},
        transactions::states::Preverified,
    },
    sdp::{Declaration, DeclarationId, ServiceType},
};
use lb_cryptarchia_engine::{Branch, Epoch, EraSchedule, PrunedBlocks, Slot};
use lb_cryptarchia_sync::{BlocksUnavailableReason, ProviderResponse, TipResponseFailureReason};
use lb_ledger::config::{config_for_slot, stake_distribution_snapshot};
use lb_log_targets::diagnostic::BLEND_REACHABILITY;
use lb_network_service::message::ChainSyncEvent;
use lb_storage_service::api::StorageApi;
use lb_utils::tokio::task::spawn_blocking;
use overwatch::{
    DynError,
    services::{relay::InboundRelay, state::StateUpdater},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{
    AppliedBlock, BlockOrigin, ChainServiceInfo, ConsensusMsg, Cryptarchia,
    CryptarchiaConsensusState, EpochStateQueryResult, Error, LOG_TARGET, LibUpdate,
    ProcessedBlockEvent, PrunedBlocksInfo, Query, TryApplyBlockOutcome, metrics,
    notifier::ChainOnlineNotifier,
    relays::{BroadcastRelay, CryptarchiaConsensusRelays},
    sync::block_provider::BlockProvider,
};

pub struct ProcessBlockOutcome<Tx> {
    pub pruned_blocks: PrunedBlocks<HeaderId>,
    pub reorged_block_ids: Vec<HeaderId>,
    pub reorged_txs: Vec<TxInBlock<Tx>>,
    pub newly_canonical_txs: HashMap<TxHash, Slot>,
}

/// Where the outcome of applying a block goes.
type ApplyBlockReply<Tx> = oneshot::Sender<Result<AppliedBlock<Tx>, Error>>;

/// What became of a block off the service loop, for the loop to finish.
struct BlockPreparation<Tx> {
    prepared: Result<PreparedBlock<Tx>, Error>,
    /// The blocks that are yet to be removed from storage.
    storage_blocks_to_remove: HashSet<HeaderId>,
    reply_channel: ApplyBlockReply<Tx>,
}

/// A block that passed verification on a copy of the chain, which holds the
/// block's changes until it's committed.
struct VerifiedBlock<Tx> {
    block: Block<Tx>,
    candidate: Cryptarchia,
    applied: TryApplyBlockOutcome,
}

/// A verified and stored block, with all it takes to commit it.
struct PreparedBlock<Tx> {
    candidate: Cryptarchia,
    block_id: HeaderId,
    block_slot: Slot,
    tx_count: usize,
    outcome: ProcessBlockOutcome<Tx>,
}

// Source tips normally leave this map when they pass the LIB. These small
// limits also protect the diagnostic path during a long LIB stall.
const MAX_QUERY_SOURCES_PER_TIP: usize = 8;
const MAX_QUERY_SOURCE_TIPS: usize = 64;

/// Shared implementation for the live-tip and finalized-LIB declaration
/// queries. The test fixture exercises these same methods against two state
/// snapshots, so a query cannot accidentally bypass the tip/LIB selection.
trait SdpQuerySource {
    type Declaration: Clone;

    fn tip_state_id(&self) -> HeaderId;
    fn lib_state_id(&self) -> HeaderId;
    fn live_declarations_at(&self, state_id: HeaderId)
    -> HashMap<DeclarationId, Self::Declaration>;
    fn finalized_declaration_at(
        &self,
        state_id: HeaderId,
        id: &DeclarationId,
    ) -> Option<Self::Declaration>;
    fn finalized_declarations_at(
        &self,
        state_id: HeaderId,
        service_type: ServiceType,
    ) -> Option<HashMap<DeclarationId, Self::Declaration>>;

    fn live_sdp_declarations(&self) -> HashMap<DeclarationId, Self::Declaration> {
        self.live_declarations_at(self.tip_state_id())
    }

    fn finalized_sdp_declaration(&self, id: &DeclarationId) -> Option<Self::Declaration> {
        self.finalized_declaration_at(self.lib_state_id(), id)
    }

    fn finalized_sdp_declarations(
        &self,
        service_type: ServiceType,
    ) -> Option<HashMap<DeclarationId, Self::Declaration>> {
        self.finalized_declarations_at(self.lib_state_id(), service_type)
    }
}

impl SdpQuerySource for Cryptarchia {
    type Declaration = Declaration;

    fn tip_state_id(&self) -> HeaderId {
        self.tip()
    }

    fn lib_state_id(&self) -> HeaderId {
        self.lib()
    }

    fn live_declarations_at(
        &self,
        state_id: HeaderId,
    ) -> HashMap<DeclarationId, Self::Declaration> {
        self.ledger
            .state(&state_id)
            .map(|ledger_state| {
                ledger_state
                    .mantle_ledger()
                    .sdp
                    .declarations()
                    .iter()
                    .flat_map(|(_, declarations)| {
                        declarations
                            .iter()
                            .map(|(id, declaration)| (*id, declaration.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn finalized_declaration_at(
        &self,
        state_id: HeaderId,
        id: &DeclarationId,
    ) -> Option<Self::Declaration> {
        self.ledger
            .state(&state_id)?
            .mantle_ledger()
            .sdp
            .get_declaration(id)
            .cloned()
    }

    fn finalized_declarations_at(
        &self,
        state_id: HeaderId,
        service_type: ServiceType,
    ) -> Option<HashMap<DeclarationId, Self::Declaration>> {
        let declarations = self
            .ledger
            .state(&state_id)?
            .mantle_ledger()
            .sdp
            .get_declarations_by_service(service_type)?;
        Some(
            declarations
                .iter()
                .map(|(id, declaration)| (*id, declaration.clone()))
                .collect(),
        )
    }
}

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
pub struct Service<Phase, Tx>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTransaction + Clone + Eq + Debug,
{
    phase: Phase,
    cryptarchia: Cryptarchia,
    inbound_relay: InboundRelay<ConsensusMsg<Tx>>,
    state_updater: StateUpdater<Option<CryptarchiaConsensusState>>,
    fork_digests: Arc<EraSchedule<ForkDigest>>,
    new_block_subscription_sender: broadcast::Sender<ProcessedBlockEvent>,
    lib_subscription_sender: broadcast::Sender<LibUpdate>,
    chain_online_notifier: ChainOnlineNotifier,
    current_slot: Slot,
    storage_blocks_to_remove: HashSet<HeaderId>,
    relays: CryptarchiaConsensusRelays<Tx>,
    sync_blocks_provider: BlockProvider<Tx>,
    slot_timer: lb_time_service::EpochSlotTickStream,
    state_recording_timer: tokio::time::Interval,
    prolonged_bootstrap_period: Duration,
    epoch_state_query_sources: EpochStateQuerySourceTracker,
    /// The block being prepared, if any, which the service loop polls along
    /// with its other inputs.
    block_underway: Option<Pin<Box<dyn Future<Output = BlockPreparation<Tx>> + Send>>>,
    /// The blocks waiting for it.
    queued_blocks: VecDeque<(Block<Tx>, ApplyBlockReply<Tx>)>,
    /// The queries waiting on storage, which the service loop polls along with
    /// its other inputs.
    pending_queries: FuturesUnordered<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl<Phase, Tx> Service<Phase, Tx>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    /// Move to the `NextPhase`, carrying all the shared ingredients over.
    fn with_phase<NextPhase: phases::Phase>(self, phase: NextPhase) -> Service<NextPhase, Tx> {
        Service {
            phase,
            cryptarchia: self.cryptarchia,
            inbound_relay: self.inbound_relay,
            state_updater: self.state_updater,
            fork_digests: self.fork_digests,
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
            block_underway: self.block_underway,
            queued_blocks: self.queued_blocks,
            pending_queries: self.pending_queries,
        }
    }

    /// Apply a block to the chain, once the blocks ahead of it are through, and
    /// reply with the result.
    ///
    /// A block's proofs and storage round trips are polled along with the
    /// service loop's other inputs, so it goes on serving queries and slot
    /// ticks meanwhile, and the proofs are verified on the blocking pool. They
    /// work on a copy of the chain that replaces it when the block is
    /// committed, so blocks go one at a time: of two prepared side by side, one
    /// would be lost.
    fn apply_block(&mut self, block: Block<Tx>, reply_channel: ApplyBlockReply<Tx>) {
        self.queued_blocks.push_back((block, reply_channel));
        self.prepare_next_block();
    }

    fn prepare_next_block(&mut self) {
        if self.block_underway.is_some() {
            return;
        }
        let Some((block, reply_channel)) = self.queued_blocks.pop_front() else {
            return;
        };
        let candidate = self.cryptarchia.clone();
        let current_slot = self.current_slot;
        let storage = self.relays.storage().clone();
        let blocks_to_remove = self.storage_blocks_to_remove.clone();
        self.block_underway = Some(Box::pin(async move {
            let prepared = prepare_block(
                candidate,
                block,
                current_slot,
                BlockOrigin::Network,
                &storage,
            )
            .await;
            let storage_blocks_to_remove = delete_stale_blocks_from_storage(
                prepared
                    .iter()
                    .flat_map(|prepared| prepared.outcome.pruned_blocks.stale_blocks().copied()),
                &blocks_to_remove,
                &storage,
            )
            .await;
            BlockPreparation {
                prepared,
                storage_blocks_to_remove,
                reply_channel,
            }
        }));
    }

    /// Commit the block that has been prepared, and move on to the next.
    async fn finish_block(&mut self, preparation: BlockPreparation<Tx>) {
        self.block_underway = None;
        let BlockPreparation {
            prepared,
            storage_blocks_to_remove,
            reply_channel,
        } = preparation;
        self.storage_blocks_to_remove = storage_blocks_to_remove;
        self.commit_block_and_reply(prepared, reply_channel).await;
        self.prepare_next_block();
    }

    /// Commit a prepared block to the chain and reply with the result.
    async fn commit_block_and_reply(
        &mut self,
        prepared: Result<PreparedBlock<Tx>, Error>,
        reply_channel: ApplyBlockReply<Tx>,
    ) {
        match prepared {
            Ok(prepared) => {
                let outcome = self.commit_block_and_update_state(prepared).await;
                reply_channel
                    .send(Ok(AppliedBlock {
                        tip: self.cryptarchia.tip(),
                        newly_canonical_txs: outcome.newly_canonical_txs,
                        reorged_txs: outcome.reorged_txs,
                        lib_slot: self.cryptarchia.lib_branch().slot(),
                    }))
                    .unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not send process block result through channel");
                    });
            }
            Err(e) => {
                log_process_block_error(&e);
                reply_channel.send(Err(e)).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send process block error through channel");
                });
            }
        }
    }

    /// Commit a prepared block and update the service state accordingly.
    async fn commit_block_and_update_state(
        &mut self,
        prepared: PreparedBlock<Tx>,
    ) -> ProcessBlockOutcome<Tx> {
        let previous_lib_slot = self.cryptarchia.lib_branch().slot();
        let outcome = commit_block(
            &mut self.cryptarchia,
            prepared,
            self.relays.broadcast_relay(),
            &self.new_block_subscription_sender,
            &self.lib_subscription_sender,
        )
        .await;

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

        self.record_recovery_state();

        outcome
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
                debug!(
                    target: LOG_TARGET,
                    diagnostic = BLEND_REACHABILITY,
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

    /// Serve a read-only query. Available in every phase. A query that has to
    /// wait for storage is answered by a future the service loop polls along
    /// with its other inputs.
    #[expect(clippy::too_many_lines, reason = "TODO: refactor into funcs")]
    fn process_query(&mut self, query: Query) {
        match query {
            Query::Info { reply_channel } => {
                reply_channel
                    .send(ChainServiceInfo {
                        cryptarchia_info: self.cryptarchia.info(),
                        phase: Phase::TAG,
                    })
                    .unwrap_or_else(|e| {
                        error!(target: LOG_TARGET, "Could not send consensus info through channel: {:?}", e);
                    });
            }
            Query::HighestBlock { reply_channel } => {
                let highest = self.cryptarchia.highest_branch();
                reply_channel
                    .send((highest.id(), highest.slot()))
                    .unwrap_or_else(|e| {
                        error!(target: LOG_TARGET, "Could not send the highest block through channel: {:?}", e);
                    });
            }
            Query::NewBlockSubscribe { sender } => {
                sender
                    .send(self.new_block_subscription_sender.subscribe())
                    .unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not subscribe to new block channel");
                    });
            }
            Query::LibSubscribe { sender } => {
                sender
                    .send(self.lib_subscription_sender.subscribe())
                    .unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not subscribe to LIB updates channel");
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
                    self.relays.storage().clone(),
                );
                reply_channel.send(stream).unwrap_or_else(
                    |_| error!(target: LOG_TARGET, "could not send block stream through channel"),
                );
            }
            Query::GetLedgerState {
                block_id,
                reply_channel,
            } => {
                let ledger_state = self.cryptarchia.ledger.state(&block_id).cloned();
                reply_channel.send(ledger_state).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send ledger state through channel");
                });
            }
            Query::GetLedgerConfigAtSlot {
                slot,
                reply_channel,
            } => {
                reply_channel
                    .send(config_for_slot(self.cryptarchia.ledger.era_schedule(), slot).clone())
                    .unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not send ledger config through channel");
                    });
            }
            Query::GetSdpDeclarations { reply_channel } => {
                let declarations = self.cryptarchia.live_sdp_declarations();
                reply_channel.send(declarations).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send SDP declarations through channel");
                });
            }
            Query::GetFinalizedSdpDeclaration {
                declaration_id,
                reply_channel,
            } => {
                let declaration = self.cryptarchia.finalized_sdp_declaration(&declaration_id);
                reply_channel.send(declaration).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send finalized SDP declaration through channel");
                });
            }
            Query::GetFinalizedSdpDeclarations {
                service_type,
                reply_channel,
            } => {
                let declarations = self.cryptarchia.finalized_sdp_declarations(service_type);
                reply_channel.send(declarations).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send finalized SDP declarations through channel");
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
                    error!(target: LOG_TARGET, "Could not send SDP snapshot through channel");
                });
            }
            Query::GetEpochState {
                slot,
                reply_channel,
            } => {
                let result = self.cryptarchia.epoch_state_for_slot(slot);
                reply_channel.send(result).unwrap_or_else(|_| {
                    error!(target: LOG_TARGET, "Could not send epoch state through channel");
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
                    error!(target: LOG_TARGET, "Could not send epoch state through channel");
                });
            }
            Query::GetBlockEvents { id, reply_channel } => {
                let storage = self.relays.storage().clone();
                self.pending_queries.push(Box::pin(async move {
                    let events = storage.get_block_events(&id).await;
                    reply_channel.send(events).unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not send block events through channel");
                    });
                }));
            }
            Query::SelectUncles {
                parent,
                slot,
                reply_channel,
            } => {
                let candidates = self.uncle_candidates(parent, slot);
                let max_uncles = self
                    .cryptarchia
                    .ledger
                    .era_schedule()
                    .at_slot(slot)
                    .parameters
                    .consensus_config
                    .max_uncles();
                let storage = self.relays.storage().clone();
                self.pending_queries.push(Box::pin(async move {
                    let uncles = load_uncles(&candidates, max_uncles, &storage).await;
                    reply_channel.send(uncles).unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not send uncles through channel");
                    });
                }));
            }
            Query::SubscribeChainOnline { sender } => {
                sender
                    .send(self.chain_online_notifier.subscribe())
                    .unwrap_or_else(|_| {
                        error!(target: LOG_TARGET, "Could not subscribe to new block channel");
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

    /// The blocks that a new block extending `parent` at `slot` can carry as
    /// uncles.
    fn uncle_candidates(&self, parent: HeaderId, slot: Slot) -> Vec<HeaderId> {
        let Some(parent_branch) = self.cryptarchia.consensus.branches().get(&parent) else {
            return Vec::new();
        };
        self.cryptarchia
            .consensus
            .select_uncles(parent_branch, slot)
            .into_iter()
            .map(Branch::id)
            .collect()
    }

    /// Record the current service state.
    fn record_recovery_state(&self) {
        persist_recovery_state(
            &self.cryptarchia,
            self.storage_blocks_to_remove.clone(),
            &self.fork_digests,
            &self.state_updater,
        );
    }
}

fn log_epoch_state_query(result: &EpochStateQueryResult) {
    if !tracing::enabled!(target: LOG_TARGET, tracing::Level::TRACE) {
        return;
    }
    let returned_active_declaration_count = result
        .epoch_state
        .active_declarations
        .iter()
        .map(|(_, declarations)| declarations.len())
        .sum::<usize>();

    trace!(
        target: LOG_TARGET,
        diagnostic = BLEND_REACHABILITY,
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
        diagnostic = BLEND_REACHABILITY,
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
            let inactivity_period =
                config_for_slot(cryptarchia.ledger.era_schedule(), block.header().slot())
                    .sdp_config
                    .service_params
                    .get(&new_declaration.service_type)
                    .map_or(0, |params| {
                        params.inactivity_period.into_inner().into_inner()
                    });

            info!(
                target: LOG_TARGET,
                diagnostic = BLEND_REACHABILITY,
                event = "sdp_activity_committed",
                canonical = true,
                provider_id = ?new_declaration.provider_id,
                declaration_id = %active.declaration_id,
                proof_epoch = u32::from(active.metadata.origin_epoch()),
                tx_id = %tx.hash(),
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
            diagnostic = BLEND_REACHABILITY,
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
                trace!(
                    target: LOG_TARGET,
                    diagnostic = BLEND_REACHABILITY,
                    event = provider_event,
                    canonical = true,
                    target_epoch = u32::from(target_epoch),
                    snapshot_slot = u64::from(snapshot_slot),
                    provider_id = ?snapshot_declaration.provider_id,
                    declaration_id = %declaration_id,
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
                trace!(
                    target: LOG_TARGET,
                    diagnostic = BLEND_REACHABILITY,
                    event = provider_event,
                    canonical = true,
                    target_epoch = u32::from(target_epoch),
                    snapshot_slot = u64::from(snapshot_slot),
                    provider_id = ?declaration.provider_id,
                    declaration_id = %declaration_id,
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

    for epoch_state in [
        committed_state.epoch_state(),
        committed_state.next_epoch_state(),
    ] {
        let target_epoch = epoch_state.epoch;
        if target_epoch <= Epoch::new(1) {
            continue;
        }
        let snapshot_slot = stake_distribution_snapshot(
            cryptarchia.ledger.era_schedule(),
            target_epoch
                .into_inner()
                .try_into()
                .expect("Non-genesis epoch checked."),
        );
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
pub async fn process_block<Tx>(
    cryptarchia: &mut Cryptarchia,
    block: Block<Tx>,
    current_slot: Slot,
    origin: BlockOrigin,
    relays: &CryptarchiaConsensusRelays<Tx>,
    new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
    lib_broadcaster: &broadcast::Sender<LibUpdate>,
) -> Result<ProcessBlockOutcome<Tx>, Error>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    let prepared = prepare_block(
        cryptarchia.clone(),
        block,
        current_slot,
        origin,
        relays.storage(),
    )
    .await?;
    Ok(commit_block(
        cryptarchia,
        prepared,
        relays.broadcast_relay(),
        new_block_subscription_sender,
        lib_broadcaster,
    )
    .await)
}

/// Apply `block` to `candidate`, a copy of the chain, which is what verifies
/// the block, proofs included. It's CPU work only, and it can take a while.
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip(candidate, block),
    fields(block_id = %block.header().id(), tx_count = block.transactions_iter().count(), current_slot = ?current_slot)
)]
fn verify_block<Tx>(
    mut candidate: Cryptarchia,
    block: Block<Tx>,
    current_slot: Slot,
    origin: BlockOrigin,
) -> Result<VerifiedBlock<Tx>, Error>
where
    Tx: PreverifiedMantleTransaction + StorageSize + Clone,
{
    debug!(target: LOG_TARGET, "Received proposal with ID: {:?}", block.header().id());
    let applied =
        candidate.try_apply_block_with_state_retention(block.clone(), current_slot, origin)?;
    Ok(VerifiedBlock {
        block,
        candidate,
        applied,
    })
}

/// All it takes to apply `block` to `candidate`, a copy of the chain, short of
/// committing it: verifying the block, storing it, and reading the blocks that
/// a reorg brings in and takes out.
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip(candidate, block, storage),
    fields(block_id = %block.header().id(), tx_count = block.transactions_iter().count(), current_slot = ?current_slot)
)]
async fn prepare_block<Tx>(
    candidate: Cryptarchia,
    block: Block<Tx>,
    current_slot: Slot,
    origin: BlockOrigin,
    storage: &StorageApi<Tx>,
) -> Result<PreparedBlock<Tx>, Error>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    let prev_lib = candidate.lib();
    let VerifiedBlock {
        block,
        mut candidate,
        applied:
            TryApplyBlockOutcome {
                pruned_blocks,
                reorged_blocks,
                newly_canonical_blocks,
                events,
            },
    } = spawn_blocking("logos/chain/block-verification-blocking", move || {
        verify_block(candidate, block, current_slot, origin)
    })
    .await
    .expect("Block verification should not fail")?;

    let header = block.header().clone();
    let tx_count = block.transactions_iter().count();

    let immutable_blocks = immutable_blocks_index(
        &pruned_blocks,
        Some(prev_lib),
        candidate.lib(),
        candidate.consensus.lib_branch().slot(),
    );

    storage
        .store_block_data(
            header.id(),
            header.parent(),
            block.clone(),
            events,
            immutable_blocks,
        )
        .await
        .map_err(|e| Error::Storage(format!("Failed to store block data: {e}")))?;

    let newly_canonical =
        newly_canonical_blocks_by_id(block, &newly_canonical_blocks, storage).await;
    log_newly_canonical_blocks(&candidate, &newly_canonical_blocks, &newly_canonical);
    candidate.prune_ledger_states(pruned_blocks.all());

    let newly_canonical_txs: HashMap<_, _> = newly_canonical
        .values()
        .flat_map(|block| {
            let slot = block.header().slot();
            block.transactions_iter().map(move |tx| (tx.hash(), slot))
        })
        .collect();
    let reorged_txs = reorged_txs(reorged_blocks.iter(), &newly_canonical_txs, storage)
        .await
        .collect();

    Ok(PreparedBlock {
        candidate,
        block_id: header.id(),
        block_slot: header.slot(),
        tx_count,
        outcome: ProcessBlockOutcome {
            pruned_blocks,
            reorged_block_ids: reorged_blocks.iter().copied().collect(),
            reorged_txs,
            newly_canonical_txs,
        },
    })
}

/// Replace `cryptarchia` with the copy a block was prepared on, which has to be
/// a copy of `cryptarchia` as it stands now, and tell the block's subscribers.
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip_all,
    fields(block_id = %block_id, tx_count = tx_count)
)]
async fn commit_block<Tx>(
    cryptarchia: &mut Cryptarchia,
    PreparedBlock {
        candidate,
        block_id,
        block_slot,
        tx_count,
        outcome,
    }: PreparedBlock<Tx>,
    broadcast_relay: &BroadcastRelay,
    new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
    lib_broadcaster: &broadcast::Sender<LibUpdate>,
) -> ProcessBlockOutcome<Tx> {
    let prev_lib = cryptarchia.lib();
    let new_lib = candidate.lib();

    *cryptarchia = candidate;
    metrics::emit_block_transactions_metric(tx_count);

    let processed_block_event = {
        let tip = cryptarchia.tip_branch();
        let lib = cryptarchia.lib_branch();
        ProcessedBlockEvent {
            block_id,
            block_slot,
            tip: tip.id(),
            tip_slot: tip.slot(),
            lib: lib.id(),
            lib_slot: lib.slot(),
        }
    };
    if let Err(e) = new_block_subscription_sender.send(processed_block_event) {
        debug!(target: LOG_TARGET, "No new-block subscribers to notify: {e}");
    }

    if prev_lib != new_lib {
        log_lib_advanced(
            &prev_lib,
            &new_lib,
            outcome.pruned_blocks.stale_blocks().count(),
            outcome.pruned_blocks.immutable_blocks().len(),
            outcome.reorged_block_ids.len(),
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

        if let Err(e) = broadcast_finalized_block(broadcast_relay, block_info).await {
            warn!(target: LOG_TARGET, "Failed to notify finalized-block subscribers: {e}");
        }

        let lib_update = LibUpdate {
            new_lib: cryptarchia.lib(),
            pruned_blocks: PrunedBlocksInfo {
                stale_blocks: outcome.pruned_blocks.stale_blocks().copied().collect(),
                immutable_blocks: outcome.pruned_blocks.immutable_blocks().clone(),
            },
        };

        if let Err(e) = lib_broadcaster.send(lib_update) {
            warn!(target: LOG_TARGET, "No LIB-update subscribers to notify: {e}");
        }
    }

    outcome
}

/// The headers of the `candidates`, as many of them as a block can carry, which
/// is at most `max_uncles`.
///
/// Every block accepted into the block tree is stored, so a candidate should be
/// there. If it isn't, a proposal is still valid with fewer uncles.
async fn load_uncles<Tx>(
    candidates: &[HeaderId],
    max_uncles: u8,
    storage: &StorageApi<Tx>,
) -> UncleHeaders
where
    Tx: Hashable<Hash = TxHash>
        + StorageSize
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Clone
        + Eq
        + Send
        + Sync
        + 'static,
{
    let uncles = load_blocks(candidates.iter(), storage)
        .await
        .map(|block| SignedHeader::new(block.header().clone(), *block.signature()))
        .collect();
    UncleHeaders::try_new(uncles, max_uncles)
        .expect("at most as many uncles as the era allows are selected")
}

/// Blocks of `block_ids`, loaded from storage.
async fn load_blocks<'a, Tx>(
    block_ids: impl Iterator<Item = &'a HeaderId>,
    storage: &StorageApi<Tx>,
) -> impl Iterator<Item = Block<Tx>>
where
    Tx: Hashable<Hash = TxHash>
        + StorageSize
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Clone
        + Eq
        + Send
        + Sync
        + 'static,
{
    join_all(block_ids.map(async move |block_id| {
        let block = storage.get_block(block_id).await;
        if block.is_none() {
            warn!(target: LOG_TARGET, %block_id, "Could not load a block from storage");
        }
        block
    }))
    .await
    .into_iter()
    .flatten()
}

/// The `newly_canonical_blocks` by ID, as loaded from storage.
///
/// `applied_block` is taken as it is if it's among them. This saves a storage
/// read in the common case where it extends the tip, which is the only newly
/// canonical block then.
async fn newly_canonical_blocks_by_id<Tx>(
    applied_block: Block<Tx>,
    newly_canonical_blocks: &[HeaderId],
    storage: &StorageApi<Tx>,
) -> HashMap<HeaderId, Block<Tx>>
where
    Tx: Hashable<Hash = TxHash>
        + StorageSize
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Clone
        + Eq
        + Send
        + Sync
        + 'static,
{
    let applied_block_id = applied_block.header().id();
    let mut blocks = load_blocks(
        newly_canonical_blocks
            .iter()
            .filter(move |block_id| **block_id != applied_block_id),
        storage,
    )
    .await
    .map(|block| (block.header().id(), block))
    .collect::<HashMap<_, _>>();
    if newly_canonical_blocks.contains(&applied_block_id) {
        blocks.insert(applied_block_id, applied_block);
    }
    blocks
}

/// Transactions carried by `reorged_blocks` that are not in
/// `newly_canonical_txs`, each with the slot of the block carrying it.
///
/// The newly canonical blocks often carry the same transactions as the reorged
/// blocks. Those transactions are still canonical.
async fn reorged_txs<'a, Tx>(
    reorged_blocks: impl Iterator<Item = &'a HeaderId>,
    newly_canonical_txs: &HashMap<TxHash, Slot>,
    storage: &StorageApi<Tx>,
) -> impl Iterator<Item = TxInBlock<Tx>>
where
    Tx: Hashable<Hash = TxHash>
        + StorageSize
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Clone
        + Eq
        + Send
        + Sync
        + 'static,
{
    load_blocks(reorged_blocks, storage)
        .await
        .flat_map(|block| {
            let slot = block.header().slot();
            block
                .into_transactions()
                .into_iter()
                .map(move |tx| TxInBlock { tx, slot })
        })
        .filter(move |reorged| !newly_canonical_txs.contains_key(&reorged.tx.hash()))
}

fn log_newly_canonical_blocks<Tx>(
    cryptarchia: &Cryptarchia,
    newly_canonical_blocks: &[HeaderId],
    blocks: &HashMap<HeaderId, Block<Tx>>,
) where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
{
    if !tracing::enabled!(target: LOG_TARGET, tracing::Level::INFO) {
        return;
    }

    for block_id in newly_canonical_blocks {
        let Some(canonical_block) = blocks.get(block_id) else {
            warn!(
                target: LOG_TARGET,
                diagnostic = BLEND_REACHABILITY,
                event = "canonical_diagnostic_block_unavailable",
                block_id = %block_id,
                "Could not load a newly canonical block for diagnostics"
            );
            continue;
        };
        log_canonical_sdp_activity(
            cryptarchia,
            canonical_block.header().parent(),
            canonical_block,
        );
        log_canonical_blend_snapshots(cryptarchia, canonical_block);
        log_canonical_tsi_transition(cryptarchia, canonical_block);
    }
}

/// Returns block IDs from descendant (inclusive) to ancestor
/// (inclusive) in child-to-parent order.
///
/// First tries to find blocks from memory. If any block is missing from
/// memory, it falls back to loading all subsequent blocks from storage.
pub fn get_block_ids<Tx>(
    cryptarchia: &Cryptarchia,
    from_descendant: HeaderId,
    to_ancestor: HeaderId,
    storage: StorageApi<Tx>,
) -> Pin<Box<dyn Stream<Item = Result<HeaderId, Error>> + Send>>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
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
        stream::once(async move { load_block_ids_from_storage(current, to_ancestor, storage) })
            .flatten();
    Box::pin(stream::iter(in_memory).chain(storage_stream))
}

/// Retrieves the block IDs from descendant (inclusive) to ancestor
/// (inclusive) from the storage, in child-to-parent order.
///
/// This is implemented here, and not as a method of `StorageApi`, to
/// simplify the panic and error message handling.
#[expect(closure_returning_async_block, reason = "required by try_unfold")]
pub fn load_block_ids_from_storage<Tx>(
    from_descendant: HeaderId,
    to_ancestor: HeaderId,
    storage: StorageApi<Tx>,
) -> impl Stream<Item = Result<HeaderId, Error>>
where
    Tx: PreverifiedMantleTransaction
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + BinaryEncode
        + BinaryDecode<Context = u32>
        + Send
        + Sync
        + Unpin
        + 'static,
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
pub async fn delete_stale_blocks_from_storage<Tx>(
    stale_blocks: impl Iterator<Item = HeaderId> + Send,
    additional_blocks: &HashSet<HeaderId>,
    storage: &StorageApi<Tx>,
) -> HashSet<HeaderId> {
    let blocks = stale_blocks
        .chain(additional_blocks.iter().copied())
        .collect::<HashSet<_>>();
    if blocks.is_empty() {
        return blocks;
    }

    match storage.remove_blocks(blocks.iter().copied()).await {
        Ok(()) => {
            debug!(target: LOG_TARGET, "Deleted {} stale block(s) from storage.", blocks.len());
            HashSet::new()
        }
        // We retain the blocks that failed to be deleted.
        Err(e) => {
            error!(target: LOG_TARGET, "Could not delete {} stale block(s) from storage: {e}", blocks.len());
            blocks
        }
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
        .map_err(|error| Box::new(error) as DynError)
}

/// Update and persist `CryptarchiaConsensusState`.
pub fn persist_recovery_state(
    cryptarchia: &Cryptarchia,
    storage_blocks_to_remove: HashSet<HeaderId>,
    fork_digests: &EraSchedule<ForkDigest>,
    state_updater: &StateUpdater<Option<CryptarchiaConsensusState>>,
) {
    match CryptarchiaConsensusState::from_cryptarchia_and_unpruned_blocks(
        cryptarchia,
        storage_blocks_to_remove,
        fork_digests,
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
        ChainSyncEvent::BlocksRequest { reply_sender, .. } => {
            let response = ProviderResponse::Unavailable {
                reason: BlocksUnavailableReason::Unknown,
            };
            if let Err(err) = reply_sender.send(response).await {
                error!(target: LOG_TARGET, %err, "failed to send chain sync response");
            }
        }
        ChainSyncEvent::TipRequest { reply_sender } => {
            send_chain_sync_rejection(reply_sender).await;
        }
    }
}

async fn send_chain_sync_rejection<ResponseType>(
    sender: mpsc::Sender<ProviderResponse<ResponseType, TipResponseFailureReason>>,
) {
    let response = ProviderResponse::Unavailable {
        reason: TipResponseFailureReason::NodeNotOnline,
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
    use std::{iter, sync::mpsc as std_mpsc};

    use futures::channel::mpsc as slot_tick_channel;
    use lb_core::mantle::{SignedOps, Utxo};
    use lb_cryptarchia_engine::{Era, State};
    use lb_key_management_system_keys::keys::ZkKey;
    use lb_storage_service::StorageMsg;
    use lb_time_service::SlotTick;
    use overwatch::services::relay::{OutboundRelay, Relay};
    use tempfile::TempDir;
    use tokio::{
        runtime::{Builder, Runtime},
        sync::watch,
        task::JoinHandle,
        time::{interval, sleep, timeout},
    };

    use super::{
        phases::{Following, ProlongedBootstrapPeriod},
        *,
    };
    use crate::{
        CryptarchiaInfo,
        sync::config::BlockProviderConfig,
        tests::{genesis_cryptarchia, signed_header, spawn_storage_service, try_build_block, utxo},
    };

    const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
    /// Long enough for the service to get through what it was sent.
    const SETTLE_TIME: Duration = Duration::from_millis(100);

    type TestTx = SignedOps<Preverified, StandardMode>;
    type TestBlock = Block<TestTx>;

    /// A test's end of a running [`Service`].
    struct ServiceHandle {
        relay: OutboundRelay<ConsensusMsg<TestTx>>,
        slot_ticks: slot_tick_channel::Sender<SlotTick>,
        _storage: Option<(JoinHandle<()>, TempDir)>,
    }

    impl ServiceHandle {
        async fn send(&self, message: ConsensusMsg<TestTx>) {
            assert!(self.relay.send(message).await.is_ok());
        }

        async fn apply(
            &self,
            block: TestBlock,
        ) -> oneshot::Receiver<Result<AppliedBlock<TestTx>, Error>> {
            let (reply_channel, reply) = oneshot::channel();
            assert!(
                self.relay
                    .send(ConsensusMsg::ApplyBlock {
                        block: Box::new(block),
                        reply_channel,
                    })
                    .await
                    .is_ok()
            );
            reply
        }

        async fn info(&self) -> CryptarchiaInfo {
            let (reply_channel, reply) = oneshot::channel();
            assert!(
                self.relay
                    .send(Query::Info { reply_channel }.into())
                    .await
                    .is_ok()
            );
            replied(reply).await.cryptarchia_info
        }

        fn tick(&mut self, slot: Slot) {
            self.slot_ticks
                .try_send(SlotTick {
                    era: Era::genesis(),
                    epoch: Epoch::new(0),
                    slot,
                })
                .unwrap();
        }
    }

    async fn replied<T>(reply: oneshot::Receiver<T>) -> T {
        timeout(REPLY_TIMEOUT, reply)
            .await
            .expect("the service should reply in time")
            .unwrap()
    }

    fn service_in<Phase>(
        phase: Phase,
        cryptarchia: Cryptarchia,
        current_slot: Slot,
        prolonged_bootstrap_period: Duration,
    ) -> (Service<Phase, TestTx>, ServiceHandle)
    where
        Phase: phases::Phase,
    {
        let (storage_tx, storage_rx) = mpsc::channel(16);
        let (service, handle) = service_on_storage(
            storage_tx,
            phase,
            cryptarchia,
            current_slot,
            prolonged_bootstrap_period,
        );
        let handle = ServiceHandle {
            _storage: Some(spawn_storage_service(storage_rx)),
            ..handle
        };
        (service, handle)
    }

    /// A [`Service`] whose storage is whatever answers `storage_tx`.
    fn service_on_storage<Phase>(
        storage_tx: mpsc::Sender<StorageMsg>,
        phase: Phase,
        cryptarchia: Cryptarchia,
        current_slot: Slot,
        prolonged_bootstrap_period: Duration,
    ) -> (Service<Phase, TestTx>, ServiceHandle)
    where
        Phase: phases::Phase,
    {
        let Relay {
            inbound_relay,
            outbound_relay,
            ..
        } = Relay::new(16);
        let (slot_ticks, slot_timer) = slot_tick_channel::channel(16);
        let (broadcast_tx, _) = mpsc::channel(16);
        let (time_tx, _) = mpsc::channel(16);
        let storage = StorageApi::new(OutboundRelay::new(storage_tx));
        let service = Service {
            phase,
            inbound_relay,
            state_updater: StateUpdater::new(Arc::new(watch::channel(None).0)),
            fork_digests: Arc::new(
                cryptarchia
                    .ledger
                    .era_schedule()
                    .map(|_| ForkDigest::from([0; 32])),
            ),
            new_block_subscription_sender: broadcast::channel(16).0,
            lib_subscription_sender: broadcast::channel(16).0,
            chain_online_notifier: ChainOnlineNotifier::new(*cryptarchia.consensus.state()),
            current_slot,
            storage_blocks_to_remove: HashSet::new(),
            relays: CryptarchiaConsensusRelays::new(
                OutboundRelay::new(broadcast_tx),
                storage.clone(),
                OutboundRelay::new(time_tx),
            ),
            sync_blocks_provider: BlockProvider::new(
                storage,
                BlockProviderConfig {
                    batch_size: 1.try_into().unwrap(),
                },
            ),
            slot_timer: Box::pin(slot_timer),
            state_recording_timer: interval(Duration::from_hours(1)),
            prolonged_bootstrap_period,
            epoch_state_query_sources: EpochStateQuerySourceTracker::default(),
            block_underway: None,
            queued_blocks: VecDeque::new(),
            pending_queries: FuturesUnordered::new(),
            cryptarchia,
        };
        let handle = ServiceHandle {
            relay: outbound_relay,
            slot_ticks,
            _storage: None,
        };
        (service, handle)
    }

    /// Two blocks in a row on top of `chain`.
    fn two_blocks(chain: &Cryptarchia, utxo: Utxo, key: &ZkKey) -> (TestBlock, TestBlock) {
        let block_on = |chain: &Cryptarchia, start_slot| {
            let tip = chain.tip();
            try_build_block(chain, tip, utxo, key, start_slot, UncleHeaders::empty())
                .expect("should find a winning slot")
                .0
        };
        let first = block_on(chain, Slot::new(1));
        let mut chain = chain.clone();
        chain
            .try_apply_block(first.clone(), first.header().slot())
            .unwrap();
        let second = block_on(&chain, first.header().slot().strict_add(1.into()));
        (first, second)
    }

    /// A runtime whose only blocking thread [`hold_block_verification`] can
    /// take.
    fn runtime() -> Runtime {
        Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .unwrap()
    }

    /// Takes the only blocking thread until the returned sender is dropped, so
    /// that no block can get verified.
    fn hold_block_verification() -> std_mpsc::Sender<()> {
        let (release, held) = std_mpsc::channel();
        spawn_blocking("test/hold", move || held.recv());
        release
    }

    #[test]
    fn the_service_goes_on_while_a_block_is_verified() {
        runtime().block_on(async {
            let (zk_key, utxo) = utxo();
            let chain = genesis_cryptarchia(utxo);
            let genesis = chain.tip();
            let (first, second) = two_blocks(&chain, utxo, &zk_key);
            let (second_id, second_slot) = (second.header().id(), second.header().slot());
            let (service, mut handle) =
                service_in(Following, chain, first.header().slot(), Duration::ZERO);
            tokio::spawn(service.process_following());
            let release = hold_block_verification();

            let first_applied = handle.apply(first).await;
            let second_applied = handle.apply(second).await;

            assert_eq!(handle.info().await.tip, genesis);
            // The second block would be from the future without this tick.
            handle.tick(second_slot);
            sleep(SETTLE_TIME).await;

            drop(release);
            assert!(replied(first_applied).await.is_ok());
            // On top of the first: the two went one after the other.
            assert_eq!(replied(second_applied).await.unwrap().tip, second_id);
        });
    }

    #[test]
    fn a_block_that_fails_verification_lets_the_next_one_through() {
        runtime().block_on(async {
            let (zk_key, utxo) = utxo();
            let chain = genesis_cryptarchia(utxo);
            let (first, second) = two_blocks(&chain, utxo, &zk_key);
            let first_id = first.header().id();
            let (service, handle) =
                service_in(Following, chain, second.header().slot(), Duration::ZERO);
            tokio::spawn(service.process_following());
            let release = hold_block_verification();

            // The parent of the second block is yet to come.
            let refused = handle.apply(second).await;
            let applied = handle.apply(first).await;

            drop(release);
            assert!(matches!(
                replied(refused).await,
                Err(Error::ParentMissing { .. })
            ));
            assert_eq!(replied(applied).await.unwrap().tip, first_id);
        });
    }

    #[tokio::test]
    async fn a_query_waiting_on_storage_leaves_the_service_free() {
        let (_, utxo) = utxo();
        let chain = genesis_cryptarchia(utxo);
        let genesis = chain.tip();
        let (storage_tx, mut storage_rx) = mpsc::channel(16);
        let (service, handle) = service_on_storage(
            storage_tx,
            Following,
            chain,
            Slot::genesis(),
            Duration::ZERO,
        );
        tokio::spawn(service.process_following());

        let (reply_channel, events) = oneshot::channel();
        handle
            .send(
                Query::GetBlockEvents {
                    id: genesis,
                    reply_channel,
                }
                .into(),
            )
            .await;
        let request = timeout(REPLY_TIMEOUT, storage_rx.recv()).await.unwrap();
        let Some(StorageMsg::GetBlockEvents { response_tx, .. }) = request else {
            panic!("the events should be read");
        };

        assert_eq!(handle.info().await.tip, genesis);

        response_tx.send(None).unwrap();
        assert!(replied(events).await.is_none());
    }

    #[tokio::test]
    async fn serving_chain_sync_leaves_the_service_free() {
        let (_, utxo) = utxo();
        let chain = genesis_cryptarchia(utxo);
        let genesis = chain.tip();
        let (storage_tx, mut storage_rx) = mpsc::channel(16);
        let (service, handle) = service_on_storage(
            storage_tx,
            Following,
            chain,
            Slot::genesis(),
            Duration::ZERO,
        );
        tokio::spawn(service.process_following());

        let (reply_sender, mut reply) = mpsc::channel(1);
        handle
            .send(ConsensusMsg::ChainSync(ChainSyncEvent::BlocksRequest {
                target_block: HeaderId::from([9; 32]),
                requester_tip: genesis,
                latest_immutable_block: genesis,
                additional_blocks: HashSet::new(),
                reply_sender,
            }))
            .await;
        let lookup = timeout(REPLY_TIMEOUT, storage_rx.recv())
            .await
            .unwrap()
            .expect("the blocks should be looked up");

        assert_eq!(handle.info().await.tip, genesis);

        // A storage that can't answer: the request is turned down.
        drop(lookup);
        tokio::spawn(async move { while storage_rx.recv().await.is_some() {} });
        assert!(matches!(
            timeout(REPLY_TIMEOUT, reply.recv()).await.unwrap(),
            Some(ProviderResponse::Unavailable { .. })
        ));
    }

    /// The uncle has a slot of its own: a block can't take as an uncle one in
    /// a slot its chain has filled.
    #[tokio::test]
    async fn a_fork_off_an_ancestor_is_offered_as_an_uncle() {
        let (zk_key, utxo) = utxo();
        let chain = genesis_cryptarchia(utxo);
        let genesis = chain.tip();
        let block_from = |start_slot| {
            try_build_block(
                &chain,
                genesis,
                utxo,
                &zk_key,
                start_slot,
                UncleHeaders::empty(),
            )
            .expect("should find a winning slot")
            .0
        };
        let uncle = block_from(Slot::new(1));
        let parent = block_from(uncle.header().slot().strict_add(1.into()));
        let (parent_id, next_slot) = (
            parent.header().id(),
            parent.header().slot().strict_add(1.into()),
        );
        let (service, handle) =
            service_in(Following, chain, parent.header().slot(), Duration::ZERO);
        tokio::spawn(service.process_following());
        assert!(replied(handle.apply(parent).await).await.is_ok());
        assert!(replied(handle.apply(uncle.clone()).await).await.is_ok());

        let (reply_channel, uncles) = oneshot::channel();
        handle
            .send(
                Query::SelectUncles {
                    parent: parent_id,
                    slot: next_slot,
                    reply_channel,
                }
                .into(),
            )
            .await;

        assert_eq!(
            replied(uncles).await.iter().collect::<Vec<_>>(),
            [&signed_header(&uncle)]
        );
    }

    /// Storing is the one round trip a block on the tip takes.
    #[tokio::test]
    async fn the_service_goes_on_while_a_block_is_stored() {
        let (zk_key, utxo) = utxo();
        let chain = genesis_cryptarchia(utxo);
        let genesis = chain.tip();
        let (block, _) = two_blocks(&chain, utxo, &zk_key);
        let block_id = block.header().id();
        let (storage_tx, mut storage_rx) = mpsc::channel(16);
        let (service, handle) = service_on_storage(
            storage_tx,
            Following,
            chain,
            block.header().slot(),
            Duration::ZERO,
        );
        tokio::spawn(service.process_following());

        let applied = handle.apply(block).await;
        let request = timeout(REPLY_TIMEOUT, storage_rx.recv()).await.unwrap();
        let Some(StorageMsg::StoreBlockData { response_tx, .. }) = request else {
            panic!("the block should be stored first");
        };

        assert_eq!(handle.info().await.tip, genesis);

        response_tx.send(Ok(())).unwrap();
        assert_eq!(replied(applied).await.unwrap().tip, block_id);
        assert!(storage_rx.try_recv().is_err());
    }

    /// The stale block and the one left from before, for a storage that answers
    /// `reply` to deleting them, and what the deletion leaves to remove.
    async fn deletion_answered_with(
        reply: Result<(), String>,
    ) -> (HeaderId, HeaderId, HashSet<HeaderId>) {
        let (storage_tx, mut storage_rx) = mpsc::channel(1);
        let storage = StorageApi::<TestTx>::new(OutboundRelay::new(storage_tx));
        tokio::spawn(async move {
            let Some(StorageMsg::RemoveBlocks { response_tx, .. }) = storage_rx.recv().await else {
                panic!("the blocks should be removed");
            };
            response_tx.send(reply).unwrap();
        });
        let (stale, left_from_before) = (HeaderId::from([1; 32]), HeaderId::from([2; 32]));

        let remaining = delete_stale_blocks_from_storage(
            iter::once(stale),
            &HashSet::from([left_from_before]),
            &storage,
        )
        .await;
        (stale, left_from_before, remaining)
    }

    #[tokio::test]
    async fn deleted_blocks_are_forgotten() {
        let (_, _, remaining) = deletion_answered_with(Ok(())).await;

        assert_eq!(remaining, HashSet::new());
    }

    #[tokio::test]
    async fn blocks_that_could_not_be_deleted_are_kept_for_later() {
        let (stale, left_from_before, remaining) =
            deletion_answered_with(Err("storage failure".to_owned())).await;

        assert_eq!(remaining, HashSet::from([stale, left_from_before]));
    }

    #[tokio::test]
    async fn nothing_to_delete_leaves_storage_alone() {
        let (storage_tx, mut storage_rx) = mpsc::channel(1);
        let storage = StorageApi::<TestTx>::new(OutboundRelay::new(storage_tx));

        delete_stale_blocks_from_storage(iter::empty(), &HashSet::new(), &storage).await;

        assert!(storage_rx.try_recv().is_err());
    }

    /// Going online changes the chain, and the block would undo that by
    /// replacing it with the copy it was prepared on.
    #[test]
    fn going_online_waits_for_the_block_underway() {
        runtime().block_on(async {
            let (zk_key, utxo) = utxo();
            let chain = genesis_cryptarchia(utxo);
            let (block, _) = two_blocks(&chain, utxo, &zk_key);
            let block_id = block.header().id();
            let bootstrap_period = Duration::from_millis(50);
            let (service, handle) = service_in(
                ProlongedBootstrapPeriod,
                chain,
                block.header().slot(),
                bootstrap_period,
            );
            tokio::spawn(async move {
                service
                    .process_prolonged_bootstrap_period()
                    .await
                    .process_following()
                    .await;
            });
            let release = hold_block_verification();

            let applied = handle.apply(block).await;
            sleep(bootstrap_period * 4).await;
            assert_eq!(handle.info().await.state, State::Bootstrapping);

            drop(release);
            assert!(replied(applied).await.is_ok());
            sleep(SETTLE_TIME).await;
            let info = handle.info().await;
            assert_eq!(info.state, State::Online);
            assert_eq!(info.tip, block_id);
        });
    }

    type SdpQueryFixtureStates =
        HashMap<HeaderId, HashMap<ServiceType, HashMap<DeclarationId, &'static str>>>;

    struct SdpQueryFixture {
        tip: HeaderId,
        lib: HeaderId,
        states: SdpQueryFixtureStates,
    }

    impl SdpQuerySource for SdpQueryFixture {
        type Declaration = &'static str;

        fn tip_state_id(&self) -> HeaderId {
            self.tip
        }

        fn lib_state_id(&self) -> HeaderId {
            self.lib
        }

        fn live_declarations_at(
            &self,
            state_id: HeaderId,
        ) -> HashMap<DeclarationId, Self::Declaration> {
            self.states
                .get(&state_id)
                .into_iter()
                .flat_map(HashMap::values)
                .flat_map(|declarations| declarations.iter())
                .map(|(id, declaration)| (*id, *declaration))
                .collect()
        }

        fn finalized_declaration_at(
            &self,
            state_id: HeaderId,
            id: &DeclarationId,
        ) -> Option<Self::Declaration> {
            self.states
                .get(&state_id)?
                .values()
                .find_map(|declarations| declarations.get(id).copied())
        }

        fn finalized_declarations_at(
            &self,
            state_id: HeaderId,
            service_type: ServiceType,
        ) -> Option<HashMap<DeclarationId, Self::Declaration>> {
            self.states.get(&state_id)?.get(&service_type).cloned()
        }
    }

    /// The same declaration query methods used by `Cryptarchia` read TIP for
    /// live queries and LIB for finalized queries. Missing values stay absent,
    /// while an existing empty service registry stays successful.
    #[test]
    fn finalized_sdp_queries_are_distinct_from_tip_registry_queries() {
        let id = DeclarationId([9; 32]);
        let tip_id = HeaderId::from([2; 32]);
        let lib_id = HeaderId::from([1; 32]);
        let source = SdpQueryFixture {
            tip: tip_id,
            lib: lib_id,
            states: HashMap::from([
                (
                    tip_id,
                    HashMap::from([(
                        ServiceType::BlendNetwork,
                        HashMap::from([(id, "tip declaration B")]),
                    )]),
                ),
                (
                    lib_id,
                    HashMap::from([(
                        ServiceType::BlendNetwork,
                        HashMap::from([(id, "finalized declaration A")]),
                    )]),
                ),
            ]),
        };

        assert_eq!(
            source.live_sdp_declarations().get(&id),
            Some(&"tip declaration B"),
            "the live declaration query returns TIP state B"
        );
        assert_eq!(
            source.finalized_sdp_declaration(&id),
            Some("finalized declaration A"),
            "the finalized by-ID query returns LIB state A"
        );
        assert_eq!(
            source.finalized_sdp_declarations(ServiceType::BlendNetwork),
            Some(HashMap::from([(id, "finalized declaration A")]))
        );
        assert_eq!(
            source.finalized_sdp_declaration(&DeclarationId([8; 32])),
            None,
            "a missing declaration is an absence/error, not an empty result"
        );
        let missing_service_source = SdpQueryFixture {
            tip: tip_id,
            lib: lib_id,
            states: HashMap::from([(lib_id, HashMap::new())]),
        };
        assert_eq!(
            missing_service_source.finalized_sdp_declarations(ServiceType::BlendNetwork),
            None,
            "a missing service registry is an absence/error"
        );
        let empty_service_source = SdpQueryFixture {
            tip: tip_id,
            lib: lib_id,
            states: HashMap::from([(
                lib_id,
                HashMap::from([(ServiceType::BlendNetwork, HashMap::new())]),
            )]),
        };
        assert_eq!(
            empty_service_source.finalized_sdp_declarations(ServiceType::BlendNetwork),
            Some(HashMap::<DeclarationId, &'static str>::new()),
            "an existing service with no declarations is distinct from absence"
        );
    }

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
