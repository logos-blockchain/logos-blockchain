//! The chain service and its operations, shared by all [`crate::phases`].

pub mod phases;

use core::fmt::{Debug, Display};
use std::{
    collections::{BTreeMap, HashSet},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use futures::{Stream, StreamExt as _, future::join_all, stream};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockInfo};
use lb_core::{
    block::Block,
    events::Events,
    header::HeaderId,
    mantle::{
        traits::{MantleTxWithProofs, PreverifiedMantleTx},
        transactions::GasPrices,
    },
};
use lb_cryptarchia_engine::{PrunedBlocks, Slot};
use lb_cryptarchia_sync::{BlocksUnavailableReason, ProviderResponse};
use lb_network_service::message::ChainSyncEvent;
use lb_storage_service::{api::chain::StorageChainApi, backends::StorageBackend};
use overwatch::{
    DynError,
    services::{relay::InboundRelay, state::StateUpdater},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, instrument, trace, warn};

use crate::{
    ChainServiceInfo, ConsensusMsg, Cryptarchia, CryptarchiaConsensusState, Error, LOG_TARGET,
    LibUpdate, ProcessedBlockEvent, PrunedBlocksInfo, Query, metrics,
    notifier::ChainOnlineNotifier,
    relays::{BroadcastRelay, CryptarchiaConsensusRelays},
    storage::{StorageAdapter as _, adapters::StorageAdapter},
    sync::block_provider::BlockProvider,
};

/// The chain service in the phase `P`.
pub struct Service<Phase, Tx, Storage, RuntimeServiceId>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTx + Clone + Eq + Debug,
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
}

impl<Phase, Tx, Storage, RuntimeServiceId> Service<Phase, Tx, Storage, RuntimeServiceId>
where
    Phase: phases::Phase,
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
        }
    }

    /// Apply a block to the chain and reply with the result.
    async fn apply_block_and_reply(
        &mut self,
        block: Block<Tx>,
        reply_channel: oneshot::Sender<Result<(HeaderId, Vec<Tx>), Error>>,
    ) {
        match self.process_block_and_update_state(block).await {
            Ok(reorged_txs) => {
                reply_channel
                    .send(Ok((self.cryptarchia.tip(), reorged_txs)))
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
    async fn process_block_and_update_state(&mut self, block: Block<Tx>) -> Result<Vec<Tx>, Error> {
        let (pruned_blocks, reorged_txs) = process_block(
            &mut self.cryptarchia,
            block,
            self.current_slot,
            &self.relays,
            &self.new_block_subscription_sender,
            &self.lib_subscription_sender,
        )
        .await?;

        self.storage_blocks_to_remove = delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.storage_blocks_to_remove,
            self.relays.storage_adapter(),
        )
        .await;

        self.record_recovery_state();

        Ok(reorged_txs)
    }

    /// Serve a read-only query. Available in every phase.
    #[expect(clippy::too_many_lines, reason = "TODO: refactor into funcs")]
    async fn process_query(&self, query: Query) {
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
            Query::SubscribeChainOnline { sender } => {
                sender
                    .send(self.chain_online_notifier.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to new block channel");
                    });
            }
        }
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
) -> Result<(PrunedBlocks<HeaderId>, Vec<Tx>), Error>
where
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
    let header = block.header();
    let prev_lib = cryptarchia.lib();

    let mut candidate = cryptarchia.clone();
    let (pruned_blocks, reorged_blocks, events) =
        candidate.try_apply_block(&block, current_slot)?;
    let new_lib = candidate.lib();

    let tx_count = block.transactions_iter().count();

    let immutable_blocks = immutable_blocks_index(
        &pruned_blocks,
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
            events,
            immutable_blocks,
        )
        .await
        .map_err(|e| Error::Storage(format!("Failed to store block data: {e}")))?;

    *cryptarchia = candidate;
    metrics::emit_block_transactions_metric(tx_count);

    let processed_block_event = {
        let tip = cryptarchia.tip_branch();
        let lib = cryptarchia.lib_branch();
        ProcessedBlockEvent {
            block_id: header.id(),
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
            pruned_blocks.stale_blocks().count(),
            pruned_blocks.immutable_blocks().len(),
            reorged_blocks.len(),
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
                stale_blocks: pruned_blocks.stale_blocks().copied().collect(),
                immutable_blocks: pruned_blocks.immutable_blocks().clone(),
            },
        };

        if let Err(e) = lib_broadcaster.send(lib_update) {
            warn!("No LIB-update subscribers to notify: {e}");
        }
    }

    let reorged_txs: Vec<_> = join_all(
        reorged_blocks
            .iter()
            .map(|id| relays.storage_adapter().get_block(id)),
    )
    .await
    .into_iter()
    .flatten()
    .flat_map(Block::into_transactions)
    .collect();

    Ok((pruned_blocks, reorged_txs))
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
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
    Tx: PreverifiedMantleTx
        + MantleTxWithProofs<Context = GasPrices>
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
