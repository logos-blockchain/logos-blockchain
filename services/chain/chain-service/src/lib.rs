pub mod api;
mod bootstrap;
mod metrics;
mod notifier;
mod relays;
mod service;
mod states;
pub mod storage;
mod sync;
#[cfg(test)]
mod tests;

use core::fmt::Debug;
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use derivative::Derivative;
use futures::{Stream, TryStreamExt as _};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_core::{
    block::{Block, UncleHeaders, genesis::GenesisBlock},
    events::Events,
    header::HeaderId,
    mantle::{
        gas::MainnetGasConstants,
        traits::{MantleTxWithProofs, PreverifiedMantleTx},
        transactions::GasPrices,
    },
    sdp::{Declaration, DeclarationId},
};
use lb_cryptarchia_engine::{Branch, PrunedBlocks, ReorgedBlocks, UncleSlots};
pub use lb_cryptarchia_engine::{Epoch, Slot, State};
pub use lb_ledger::EpochState;
use lb_ledger::LedgerState;
use lb_network_service::message::ChainSyncEvent;
use lb_services_utils::{
    overwatch::{RecoveryData, RecoveryOperator},
    wait_until_services_are_ready,
};
use lb_storage_service::{
    StorageService,
    api::chain::StorageChainApi,
    backends::StorageBackend,
    recovery::{StorageRecoveryBackend, StorageRecoverySettings},
};
use lb_time_service::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_with::serde_as;
use thiserror::Error;
use tokio::sync::{broadcast, oneshot, watch};
use tracing::{Level, debug, error, info, span, trace, warn};
use tracing_futures::Instrument as _;

pub use crate::{
    bootstrap::config::{BootstrapConfig, OfflineGracePeriodConfig},
    service::phases::PhaseTag,
    states::CryptarchiaConsensusState,
    sync::config::{BlockProviderConfig, SyncConfig},
};
use crate::{
    bootstrap::state::choose_engine_state,
    notifier::ChainOnlineNotifier,
    relays::CryptarchiaConsensusRelays,
    service::{
        Service, delete_stale_blocks_from_storage, load_block_ids_from_storage,
        persist_recovery_state, process_block,
    },
    storage::{StorageAdapter as _, adapters::StorageAdapter},
    sync::block_provider::BlockProvider,
};

// Limit the number of blocks returned by GetHeaders
const SERVICE_ID: &str = "Chain";

pub(crate) const LOG_TARGET: &str = "chain::service";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Missing parent while applying block {parent}, {info:?}")]
    ParentMissing {
        parent: HeaderId,
        info: Box<CryptarchiaInfo>,
    },
    #[error("Block from future slot({block_slot:?}): current_slot:{current_slot:?}")]
    FutureBlock {
        block_slot: Slot,
        current_slot: Slot,
    },
    #[error("Block {0} has already been applied")]
    AlreadyApplied(HeaderId),
    #[error("Ledger error: {0}")]
    Ledger(#[from] lb_ledger::LedgerError<HeaderId>),
    #[error("Consensus error: {0}")]
    Consensus(#[from] lb_cryptarchia_engine::Error<HeaderId>),
    #[error("Serialization error: {0}")]
    Serialisation(#[from] lb_core::codec::Error),
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Mempool error: {0}")]
    Mempool(String),
    #[error("Block header id not found: {0}")]
    HeaderIdNotFound(HeaderId),
    #[error("Parent header ID not found for child={0}")]
    ParentIdNotFound(HeaderId),
    #[error("Awaiting genesis time")]
    AwaitingGenesisTime,
}

struct InitializedCryptarchia {
    cryptarchia: Cryptarchia,
    pruned_blocks: PrunedBlocks<HeaderId>,
    fell_back_to_lib: bool,
}

struct RecoveryBlocks<Tx> {
    blocks: Vec<Block<Tx>>,
    fell_back_to_lib: bool,
}

#[derive(Derivative)]
#[derivative(Debug)]
pub enum ConsensusMsg<Tx> {
    /// Read-only queries and subscriptions.
    /// These are served in every service phase.
    Query(Query),
    /// Apply a block to the chain,
    /// and return the tip and reorged txs if successful.
    ApplyBlock {
        block: Box<Block<Tx>>,
        reply_channel: oneshot::Sender<Result<(HeaderId, Vec<Tx>), Error>>,
    },
    /// Forward chain sync events from the network to chain-service.
    /// Chain-service will handle these directly and respond via the embedded
    /// `reply_sender`.
    ChainSync(ChainSyncEvent),
    /// Notification from chain-network that Initial Block Download has
    /// completed.
    IbdCompleted,
}

impl<Tx> From<Query> for ConsensusMsg<Tx> {
    fn from(query: Query) -> Self {
        Self::Query(query)
    }
}

/// Read-only queries and subscriptions, served in every service phase.
#[derive(Derivative)]
#[derivative(Debug)]
pub enum Query {
    Info {
        reply_channel: oneshot::Sender<ChainServiceInfo>,
    },
    NewBlockSubscribe {
        sender: oneshot::Sender<broadcast::Receiver<ProcessedBlockEvent>>,
    },
    LibSubscribe {
        sender: oneshot::Sender<broadcast::Receiver<LibUpdate>>,
    },
    GetHeaders {
        from_descendant: Option<HeaderId>,
        to_ancestor: Option<HeaderId>,
        #[derivative(Debug = "ignore")]
        reply_channel: oneshot::Sender<HeaderIdStream>,
    },
    GetLedgerState {
        block_id: HeaderId,
        reply_channel: oneshot::Sender<Option<LedgerState>>,
    },
    /// Returns all declarations in the current SDP registry, not snapshot
    GetSdpDeclarations {
        reply_channel: oneshot::Sender<HashMap<DeclarationId, Declaration>>,
    },
    /// Returns the frozen SDP snapshot for the current epoch
    GetSdpSnapshot {
        reply_channel: oneshot::Sender<HashMap<DeclarationId, Declaration>>,
    },
    GetEpochState {
        slot: Slot,
        reply_channel: oneshot::Sender<Result<EpochState, Error>>,
    },
    GetEpochConfig {
        reply_channel: oneshot::Sender<(
            lb_cryptarchia_engine::EpochConfig,
            lb_cryptarchia_engine::Config,
        )>,
    },
    GetBlockEvents {
        id: HeaderId,
        reply_channel: oneshot::Sender<Option<Events>>,
    },
    /// Selects uncles for a new block extending `parent` at `slot`.
    SelectUncles {
        parent: HeaderId,
        slot: Slot,
        reply_channel: oneshot::Sender<UncleHeaders>,
    },
    /// Subscribe to be notified when the chain becomes online mode.
    /// Since chain never goes back after entering online,
    /// the notification is delivered at most once.
    /// Late subscribers are notified immediately.
    SubscribeChainOnline {
        sender: oneshot::Sender<watch::Receiver<bool>>,
    },
}

pub(crate) type HeaderIdStream =
    Pin<Box<dyn Stream<Item = Result<HeaderId, Error>> + Send + 'static>>;

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChainServiceInfo {
    pub cryptarchia_info: CryptarchiaInfo,
    pub phase: PhaseTag,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CryptarchiaInfo {
    pub lib: HeaderId,
    pub lib_slot: Slot,
    pub tip: HeaderId,
    pub slot: Slot,
    pub height: u64,
    pub state: State,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LibUpdate {
    pub new_lib: HeaderId,
    pub pruned_blocks: PrunedBlocksInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PrunedBlocksInfo {
    pub stale_blocks: Vec<HeaderId>,
    pub immutable_blocks: BTreeMap<Slot, HeaderId>,
}

/// Event emitted when a block is processed by cryptarchia.
///
/// Note: The first message after subscribing may be an initial snapshot of the
/// current state. In this case, `block_id` can equal the current `tip` and does
/// not represent a newly processed block. Clients should handle events
/// idempotently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProcessedBlockEvent {
    /// The ID of the block that was just processed.
    pub block_id: HeaderId,
    /// The current canonical tip after processing this block.
    pub tip: HeaderId,
    pub tip_slot: Slot,
    /// The current Last Irreversible Block after processing this block.
    pub lib: HeaderId,
    pub lib_slot: Slot,
}

impl PrunedBlocksInfo {
    /// Returns an iterator over all pruned blocks, both stale and immutable.
    pub fn all(&self) -> impl Iterator<Item = HeaderId> + '_ {
        self.stale_blocks
            .iter()
            .chain(self.immutable_blocks.values())
            .copied()
    }
}

fn log_pruned_ledger_states(pruned_states_count: usize) {
    if pruned_states_count <= 1 {
        trace!(target: LOG_TARGET, "Pruned {pruned_states_count} old forks and their ledger states.");
    } else {
        debug!(target: LOG_TARGET, "Pruned {pruned_states_count} old forks and their ledger states.");
    }
}

#[derive(Clone)]
pub struct Cryptarchia {
    pub ledger: lb_ledger::Ledger<HeaderId>,
    pub consensus: lb_cryptarchia_engine::Cryptarchia<HeaderId>,
    pub genesis_id: HeaderId,
}

impl Cryptarchia {
    /// Initialize a new [`Cryptarchia`] instance.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "TODO: define Lib struct to reduce args"
    )]
    pub fn from_lib(
        lib_id: HeaderId,
        lib_ledger_state: LedgerState,
        genesis_id: HeaderId,
        ledger_config: lb_ledger::Config,
        state: State,
        lib_slot: Slot,
        lib_length: u64,
        lib_uncle_slots: UncleSlots,
    ) -> Self {
        Self {
            consensus: <lb_cryptarchia_engine::Cryptarchia<_>>::from_lib(
                lib_id,
                ledger_config.consensus_config.clone(),
                state,
                lib_slot,
                lib_length,
                lib_uncle_slots,
            ),
            ledger: <lb_ledger::Ledger<_>>::new(lib_id, lib_ledger_state, ledger_config),
            genesis_id,
        }
    }

    #[must_use]
    pub fn info(&self) -> CryptarchiaInfo {
        let tip_branch = self.tip_branch();
        let lib_branch = self.lib_branch();

        CryptarchiaInfo {
            lib: lib_branch.id(),
            lib_slot: lib_branch.slot(),
            tip: tip_branch.id(),
            slot: tip_branch.slot(),
            height: tip_branch.length(),
            state: *self.state(),
        }
    }

    #[must_use]
    pub const fn tip(&self) -> HeaderId {
        self.consensus.tip()
    }

    #[must_use]
    pub const fn tip_branch(&self) -> &Branch<HeaderId> {
        self.consensus.tip_branch()
    }

    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.consensus.lib()
    }

    #[must_use]
    pub fn lib_branch(&self) -> &Branch<HeaderId> {
        self.consensus.lib_branch()
    }

    /// Try to apply a block to the chain.
    fn try_apply_block<'tx, Tx>(
        &mut self,
        block: &Block<Tx>,
        current_slot: Slot,
    ) -> Result<(PrunedBlocks<HeaderId>, ReorgedBlocks<HeaderId>, Events), Error>
    where
        Tx: PreverifiedMantleTx + 'tx + MantleTxWithProofs<Context = GasPrices>,
    {
        let header = block.header();
        let id = header.id();
        let parent = header.parent();
        let slot = header.slot();

        if self.ledger.state(&id).is_some() {
            return Err(Error::AlreadyApplied(id));
        }

        // Reject blocks from future slots
        if slot > current_slot {
            return Err(Error::FutureBlock {
                block_slot: slot,
                current_slot,
            });
        }

        // A block number of this block if it's applied to the chain.
        let (_, state, events) = self
            .ledger
            .prepare_update::<_, _, MainnetGasConstants>(
                id,
                parent,
                slot,
                header.leader_proof(),
                block.transactions_iter(),
            )
            .map_err(|err| match err {
                lb_ledger::LedgerError::ParentNotFound(parent) => Error::ParentMissing {
                    parent,
                    info: Box::new(self.info()),
                },
                err => Error::Ledger(err),
            })?;

        let (pruned_blocks, reorged_blocks) = self
            .consensus
            .receive_block(id, parent, slot, block.uncle_headers().slots())
            .map_err(|err| match err {
                lb_cryptarchia_engine::Error::ParentMissing(parent) => Error::ParentMissing {
                    parent,
                    info: Box::new(self.info()),
                },
                err => Error::Consensus(err),
            })?;

        self.ledger.commit_update(id, state);

        // Prune the ledger states of all the pruned blocks.
        self.prune_ledger_states(pruned_blocks.all());

        metrics::emit_consensus_metrics(&self.consensus, &self.ledger);
        metrics::emit_block_imported_metric();
        Ok((pruned_blocks, reorged_blocks, events))
    }

    fn epoch_state_for_slot(&self, slot: Slot) -> Result<EpochState, Error> {
        let tip = self.tip();
        let state = self.ledger.state(&tip).expect("no state for tip");
        Ok(state.epoch_state_for_slot(slot, self.ledger.config())?)
    }

    /// Remove the ledger states associated with blocks that have been pruned by
    /// the [`lb_cryptarchia_engine::Cryptarchia`].
    ///
    /// Details on which blocks are pruned can be found in the
    /// [`lb_cryptarchia_engine::Cryptarchia::receive_block`].
    fn prune_ledger_states<'a>(&'a mut self, blocks: impl Iterator<Item = &'a HeaderId>) {
        let mut pruned_states_count = 0usize;
        for block in blocks {
            if self.ledger.prune_state_at(block) {
                pruned_states_count = pruned_states_count.saturating_add(1);
            } else {
                error!(
                   target: LOG_TARGET,
                    "Failed to prune ledger state for block {:?} which should exist.",
                    block
                );
            }
        }
        log_pruned_ledger_states(pruned_states_count);
    }

    fn online(self) -> (Self, PrunedBlocks<HeaderId>) {
        let (consensus, pruned_blocks) = self.consensus.online();
        let mut cryptarchia = Self {
            ledger: self.ledger,
            consensus,
            genesis_id: self.genesis_id,
        };

        // Prune the ledger states of all the pruned blocks.
        cryptarchia.prune_ledger_states(pruned_blocks.all());

        (cryptarchia, pruned_blocks)
    }

    const fn is_bootstrapping(&self) -> bool {
        self.consensus.state().is_bootstrapping()
    }

    const fn state(&self) -> &State {
        self.consensus.state()
    }

    #[must_use]
    pub fn has_block(&self, block_id: &HeaderId) -> bool {
        self.consensus.branches().get(block_id).is_some()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CryptarchiaSettings {
    pub config: lb_ledger::Config,
    pub starting_state: StartingState,
    pub bootstrap: BootstrapConfig,
    pub sync: SyncConfig,
    #[serde(skip)]
    pub recovery_data: RecoveryData,
}

impl StorageRecoverySettings for CryptarchiaSettings {
    const RECOVERY_KEY_SUFFIX: &'static [u8] = b"cryptarchia";

    fn recovery_data(&self) -> &RecoveryData {
        &self.recovery_data
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum StartingState {
    Genesis {
        genesis_block: Box<GenesisBlock>,
    },
    Lib {
        lib_id: HeaderId,
        lib_ledger_state: Box<LedgerState>,
        genesis_id: HeaderId,
    },
}

impl From<GenesisBlock> for StartingState {
    fn from(genesis_block: GenesisBlock) -> Self {
        Self::Genesis {
            genesis_block: Box::new(genesis_block),
        }
    }
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    TimeBackend: lb_time_service::backends::TimeBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    new_block_subscription_sender: broadcast::Sender<ProcessedBlockEvent>,
    lib_subscription_sender: broadcast::Sender<LibUpdate>,
    state: <Self as ServiceData>::State,
}

impl<Tx, Storage, TimeBackend, RuntimeServiceId> ServiceData
    for CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    TimeBackend: lb_time_service::backends::TimeBackend,
{
    type Settings = CryptarchiaSettings;
    type State = CryptarchiaConsensusState;
    type StateOperator = RecoveryOperator<
        StorageRecoveryBackend<Self::State, Self::Settings, Storage, RuntimeServiceId>,
    >;
    type Message = ConsensusMsg<Tx>;
}

#[async_trait::async_trait]
impl<Tx, Storage, TimeBackend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
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
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<BlockBroadcastService<RuntimeServiceId>>
        + AsServiceId<StorageService<Storage, RuntimeServiceId>>
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let (new_block_subscription_sender, _) = broadcast::channel(16);
        let (lib_subscription_sender, _) = broadcast::channel(16);

        Ok(Self {
            service_resources_handle,
            new_block_subscription_sender,
            lib_subscription_sender,
            state: initial_state,
        })
    }

    async fn run(self) -> Result<(), DynError> {
        let relays: CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId> =
            CryptarchiaConsensusRelays::from_service_resources_handle::<TimeBackend>(
                &self.service_resources_handle,
            )
            .await;

        let CryptarchiaSettings {
            config: ledger_config,
            bootstrap: bootstrap_config,
            starting_state,
            sync: sync_config,
            ..
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_mins(1)),
            BlockBroadcastService<_>,
            StorageService<_, _>,
            TimeService<_, _>
        )
        .await?;

        let (current_slot, slot_timer) = Self::get_slot_timer(&relays).await?;

        let InitializedCryptarchia {
            cryptarchia,
            pruned_blocks,
            fell_back_to_lib,
        } = Self::initialize_cryptarchia(
            &self.state,
            &bootstrap_config,
            ledger_config.clone(),
            &relays,
            &self.new_block_subscription_sender,
            &self.lib_subscription_sender,
            current_slot,
        )
        .await;

        // These are blocks that have been pruned by the cryptarchia engine but have not
        // yet been deleted from the storage layer.
        let storage_blocks_to_remove = delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.state.storage_blocks_to_remove,
            relays.storage_adapter(),
        )
        .await;

        if fell_back_to_lib {
            persist_recovery_state(
                &cryptarchia,
                storage_blocks_to_remove.clone(),
                &self.service_resources_handle.state_updater,
            );
        }

        let sync_blocks_provider: BlockProvider<_, _> = BlockProvider::new(
            relays.storage_adapter().storage_relay.clone(),
            sync_config.block_provider,
        );

        // Start the timer for periodic state recording for offline grace period
        let state_recording_timer = tokio::time::interval(
            bootstrap_config
                .offline_grace_period
                .state_recording_interval,
        );

        let chain_online_notifier = ChainOnlineNotifier::new(*cryptarchia.state());

        // Mark the service as ready. The service is operational and can handle requests
        // even while in bootstrap mode waiting for IBD+PBP to complete.
        self.notify_service_ready();

        let Self {
            service_resources_handle,
            new_block_subscription_sender,
            lib_subscription_sender,
            ..
        } = self;
        let service = Service::new(
            starting_state,
            cryptarchia,
            service_resources_handle.inbound_relay,
            service_resources_handle.state_updater,
            new_block_subscription_sender,
            lib_subscription_sender,
            chain_online_notifier,
            current_slot,
            storage_blocks_to_remove,
            relays,
            sync_blocks_provider,
            slot_timer,
            state_recording_timer,
            bootstrap_config.prolonged_bootstrap_period,
        );

        // Run all phases in order. Each phase consumes the service and
        // returns it in the next phase, so phases can only advance
        // one-directionally. Phases that don't apply run as no-ops.
        let run_service = async {
            service
                .process_awaiting_genesis_time()
                .await
                .process_initial_block_download()
                .await
                .process_prolonged_bootstrap_period()
                .await
                .process_following()
                .await;
        };

        // It sucks to use `SERVICE_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        run_service
            .instrument(span!(Level::TRACE, SERVICE_ID))
            .await;

        Ok(())
    }
}

impl<Tx, Storage, TimeBackend, RuntimeServiceId>
    CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
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
    TimeBackend: lb_time_service::backends::TimeBackend,
    RuntimeServiceId: Display + AsServiceId<Self> + 'static,
{
    fn notify_service_ready(&self) {
        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );
    }

    /// Get current slot and slot timer from time service.
    async fn get_slot_timer(
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
    ) -> Result<(Slot, lb_time_service::EpochSlotTickStream), DynError> {
        let slot_timer = {
            let (sender, receiver) = oneshot::channel();
            relays
                .time_relay()
                .send(lb_time_service::TimeServiceMessage::Subscribe { sender })
                .await
                .expect("Request time subscription to time service should succeed");
            receiver.await?
        };

        // TODO: Improve Subscribe API to return current slot immediately,
        // so we don't need to call CurrentSlot API separately.
        let current_slot = {
            let (sender, receiver) = oneshot::channel();
            relays
                .time_relay()
                .send(lb_time_service::TimeServiceMessage::CurrentSlot { sender })
                .await
                .expect("Request current slot from time service should succeed");
            receiver.await?.slot
        };

        Ok((current_slot, slot_timer))
    }

    async fn load_recovery_blocks_from_storage(
        tip: HeaderId,
        lib: HeaderId,
        storage: StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<Vec<Block<Tx>>, Error> {
        let ids = load_block_ids_from_storage(tip, lib, storage.clone())
            .try_collect::<Vec<_>>()
            .await?;

        let mut blocks = Vec::new();
        // `load_block_ids_from_storage` walks from tip back to LIB and includes LIB.
        // Replay recovery blocks in LIB->tip order, skipping LIB because Cryptarchia
        // is already initialized from it.
        for id in ids.into_iter().rev().skip(1) {
            let block = storage
                .get_block(&id)
                .await
                .ok_or(Error::HeaderIdNotFound(id))?;
            blocks.push(block);
        }

        Ok(blocks)
    }

    async fn load_recovery_blocks_or_fall_back_to_lib(
        tip: HeaderId,
        lib: HeaderId,
        storage: StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> RecoveryBlocks<Tx> {
        if tip == lib {
            // Cryptarchia already starts from LIB, so there is no branch to replay.
            return RecoveryBlocks {
                blocks: Vec::new(),
                fell_back_to_lib: false,
            };
        }

        match Self::load_recovery_blocks_from_storage(tip, lib, storage).await {
            Ok(blocks) => RecoveryBlocks {
                blocks,
                fell_back_to_lib: false,
            },
            Err(error @ (Error::ParentIdNotFound(_) | Error::HeaderIdNotFound(_))) => {
                warn!(
                    target: LOG_TARGET, ?tip, ?lib, ?error,
                    "could not reconstruct recovered tip branch from storage; falling back to recovered LIB",
                );

                RecoveryBlocks {
                    blocks: Vec::new(),
                    fell_back_to_lib: true,
                }
            }
            Err(error) => {
                panic!(
                    "failed to load recovery blocks from storage during initialization: {error:?}"
                );
            }
        }
    }

    /// Initialize cryptarchia
    /// It initialize cryptarchia from the LIB (initially genesis) +
    /// (optionally) known blocks which were received before the service
    /// restarted.
    ///
    /// # Arguments
    ///
    /// * `bootstrap_config` - The bootstrap configuration.
    /// * `ledger_config` - The ledger configuration.
    /// * `relays` - The relays object containing all the necessary relays for
    ///   the consensus.
    #[expect(
        clippy::cognitive_complexity,
        reason = "TODO: address this in a dedicated refactor"
    )]
    async fn initialize_cryptarchia(
        recovery_state: &CryptarchiaConsensusState,
        bootstrap_config: &BootstrapConfig,
        ledger_config: lb_ledger::Config,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
        lib_subscription_sender: &broadcast::Sender<LibUpdate>,
        current_slot: Slot,
    ) -> InitializedCryptarchia {
        info!(
            target: LOG_TARGET, tip = ?recovery_state.tip, lib = ?recovery_state.lib, lib_height = recovery_state.lib_block_length, genesis = ?recovery_state.genesis_id,
            "recovering Cryptarchia",
        );

        let lib_id = recovery_state.lib;
        let genesis_id = recovery_state.genesis_id;
        let state = choose_engine_state(
            lib_id,
            genesis_id,
            bootstrap_config,
            recovery_state.last_engine_state.as_ref(),
        );
        let mut cryptarchia = Cryptarchia::from_lib(
            lib_id,
            recovery_state.lib_ledger_state.clone(),
            genesis_id,
            ledger_config,
            state,
            recovery_state.lib_block_slot,
            recovery_state.lib_block_length,
            recovery_state.lib_block_uncle_slots.clone(),
        );

        // Stream the already applied state.
        let init_tip = cryptarchia.tip_branch();
        let init_event = {
            let lib = cryptarchia.lib_branch();
            ProcessedBlockEvent {
                block_id: init_tip.id(),
                tip: init_tip.id(),
                tip_slot: init_tip.slot(),
                lib: lib.id(),
                lib_slot: lib.slot(),
            }
        };
        if let Err(e) = new_block_subscription_sender.send(init_event) {
            debug!("No new-block subscribers to notify: {e}");
        }

        // Phase 1: Collect and load blocks in (LIB, tip].
        info!(
            target: LOG_TARGET, lib = ?lib_id, tip = ?recovery_state.tip,
            "loading stored blocks for chain recovery",
        );
        let RecoveryBlocks {
            blocks,
            fell_back_to_lib,
        } = Self::load_recovery_blocks_or_fall_back_to_lib(
            recovery_state.tip,
            lib_id,
            relays.storage_adapter().clone(),
        )
        .await;
        info!(
            target: LOG_TARGET,
            "found {} stored blocks to replay during chain recovery",
            blocks.len()
        );

        // Phase 2: Apply each block in lib->tip order.
        let mut pruned_blocks = PrunedBlocks::new();
        let n_blocks = blocks.len();
        for (i, block) in blocks.into_iter().enumerate() {
            match process_block(
                &mut cryptarchia,
                block,
                current_slot,
                relays,
                new_block_subscription_sender,
                lib_subscription_sender,
            )
            .await
            {
                Ok((new_pruned_blocks, _)) => {
                    debug!(target: LOG_TARGET, "{}/{} blocks applied during initialization", i + 1, n_blocks);
                    pruned_blocks.extend(&new_pruned_blocks);
                }
                Err(e) => {
                    error!(target: LOG_TARGET, "Error processing block: {:?}", e);
                }
            }
        }

        info!(
            target: LOG_TARGET, tip_height = cryptarchia.consensus.tip_branch().length(), lib_height = cryptarchia.consensus.lib_branch().length(),
            "{n_blocks} blocks replayed. Chain recovery finished",
        );

        InitializedCryptarchia {
            cryptarchia,
            pruned_blocks,
            fell_back_to_lib,
        }
    }
}
