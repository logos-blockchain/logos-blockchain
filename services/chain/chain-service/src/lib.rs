pub mod api;
mod bootstrap;
mod metrics;
mod notifier;
mod relays;
mod states;
pub mod storage;
mod sync;
#[cfg(test)]
mod tests;

use core::fmt::Debug;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use derivative::Derivative;
use futures::{Stream, StreamExt as _, TryStreamExt as _, future::join_all, stream};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockBroadcastService, BlockInfo};
use lb_core::{
    block::{Block, genesis::GenesisBlock},
    events::Events,
    header::HeaderId,
    mantle::{
        gas::MainnetGasConstants,
        traits::{GenesisTx as _, MantleTxWithProofs, PreverifiedMantleTx},
        transactions::GasPrices,
    },
    sdp::{Declaration, DeclarationId},
};
use lb_cryptarchia_engine::{Branch, PrunedBlocks, ReorgedBlocks};
pub use lb_cryptarchia_engine::{Epoch, Slot, State};
use lb_cryptarchia_sync::{BlocksUnavailableReason, GetTipResponse, ProviderResponse};
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
    services::{AsServiceId, ServiceCore, ServiceData, relay::InboundRelay, state::StateUpdater},
};
use relays::BroadcastRelay;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_with::serde_as;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tracing::{Level, debug, error, info, instrument, span, trace, warn};
use tracing_futures::Instrument as _;

pub use crate::{
    bootstrap::config::{BootstrapConfig, OfflineGracePeriodConfig},
    states::CryptarchiaConsensusState,
    sync::config::{BlockProviderConfig, SyncConfig},
};
use crate::{
    bootstrap::state::choose_engine_state,
    notifier::ChainOnlineNotifier,
    relays::CryptarchiaConsensusRelays,
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

/// The phase of the chain service, which advances one-directionally
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChainServicePhase {
    /// The genesis time is in the future. Only read-only queries are served.
    AwaitingGenesisTime,
    /// Applying blocks while waiting for Initial Block Download to complete.
    InitialBlockDownload,
    /// The Prolonged Bootstrap Period is running.
    ProlongedBootstrapPeriod,
    /// Following the chain in real time
    Following,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChainServiceInfo {
    pub cryptarchia_info: CryptarchiaInfo,
    pub phase: ChainServicePhase,
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

#[derive(Clone)]
pub struct Cryptarchia {
    pub ledger: lb_ledger::Ledger<HeaderId>,
    pub consensus: lb_cryptarchia_engine::Cryptarchia<HeaderId>,
    pub genesis_id: HeaderId,
}

impl Cryptarchia {
    /// Initialize a new [`Cryptarchia`] instance.
    #[must_use]
    pub fn from_lib(
        lib_id: HeaderId,
        lib_ledger_state: LedgerState,
        genesis_id: HeaderId,
        ledger_config: lb_ledger::Config,
        state: State,
        lib_slot: Slot,
        lib_length: u64,
    ) -> Self {
        Self {
            consensus: <lb_cryptarchia_engine::Cryptarchia<_>>::from_lib(
                lib_id,
                ledger_config.consensus_config.clone(),
                state,
                lib_slot,
                lib_length,
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
            .receive_block(id, parent, slot)
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
        } = Runtime::initialize_cryptarchia(
            &self,
            &bootstrap_config,
            ledger_config.clone(),
            &relays,
            current_slot,
        )
        .await;

        // These are blocks that have been pruned by the cryptarchia engine but have not
        // yet been deleted from the storage layer.
        let storage_blocks_to_remove = Runtime::delete_stale_blocks_from_storage(
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
        let runner = Runtime {
            inbound_relay: service_resources_handle.inbound_relay,
            state_updater: service_resources_handle.state_updater,
            new_block_subscription_sender,
            lib_subscription_sender,
            cryptarchia,
            chain_online_notifier,
            current_slot,
            storage_blocks_to_remove,
            relays,
            sync_blocks_provider,
            slot_timer,
            state_recording_timer,
            prolonged_bootstrap_period: bootstrap_config.prolonged_bootstrap_period,
        };

        // It sucks to use `SERVICE_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        runner
            .run(starting_state)
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
    <Storage as StorageChainApi>::Block: TryFrom<Block<Tx>> + TryInto<Block<Tx>>,
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
}

/// The runtime of the chain service,
/// which holds everything shared by all phases.
struct Runtime<Tx, Storage, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
{
    inbound_relay: InboundRelay<ConsensusMsg<Tx>>,
    state_updater: StateUpdater<Option<CryptarchiaConsensusState>>,
    new_block_subscription_sender: broadcast::Sender<ProcessedBlockEvent>,
    lib_subscription_sender: broadcast::Sender<LibUpdate>,
    cryptarchia: Cryptarchia,
    chain_online_notifier: ChainOnlineNotifier,
    current_slot: Slot,
    storage_blocks_to_remove: HashSet<HeaderId>,
    relays: CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
    sync_blocks_provider: BlockProvider<Storage, Tx>,
    slot_timer: lb_time_service::EpochSlotTickStream,
    state_recording_timer: tokio::time::Interval,
    prolonged_bootstrap_period: Duration,
}

impl<Tx, Storage, RuntimeServiceId> Runtime<Tx, Storage, RuntimeServiceId>
where
    Tx: PreverifiedMantleTx
        + AuthenticatedMantleTx<Context = GasPrices>
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
    /// Run all phases in order.
    async fn run(mut self, starting_state: StartingState) {
        // Run `AwaitingGenesisTime` phase.
        // `IbdCompleted` can be received during this phase if IBD has been skipped.
        let ibd_skipped = if let Some(genesis_timer) = Self::create_genesis_timer(starting_state) {
            self.phase_awaiting_genesis_time(genesis_timer).await
        } else {
            false
        };

        // Run `InitialBlockDownload` phase.
        if !ibd_skipped {
            self.phase_initial_block_download().await;
        }

        // Run `ProlongedBootstrapPeriod` phase
        if self.cryptarchia.is_bootstrapping() {
            self = self.phase_prolonged_bootstrap_period().await;
        }

        // Run `Following` phase.
        self.phase_following().await;
    }

    /// Creates a timer that fires when the genesis time is reached,
    /// if the starting state is `Genesis` and the genesis time is in the
    /// future.
    /// Returns `None`, otherwise.
    fn create_genesis_timer(starting_state: StartingState) -> Option<Pin<Box<tokio::time::Sleep>>> {
        if let StartingState::Genesis { genesis_block } = starting_state {
            let genesis_time: OffsetDateTime = genesis_block
                .genesis_tx()
                .cryptarchia_parameter()
                .genesis_time
                .into();
            let now = OffsetDateTime::now_utc();

            if genesis_time > now {
                info!(target: LOG_TARGET, %genesis_time, "genesis time is in the future");
                let delay = (genesis_time - now).try_into().unwrap_or_default();
                return Some(Box::pin(tokio::time::sleep(delay)));
            }
        }
        None
    }

    /// `AwaitingGenesisTime` phase: Wait until the genesis time is reached.
    /// Returns whether IBD has been skipped during this phase.
    #[expect(clippy::cognitive_complexity, reason = "better in one fn")]
    async fn phase_awaiting_genesis_time(
        &mut self,
        mut genesis_timer: Pin<Box<tokio::time::Sleep>>,
    ) -> bool {
        let phase = ChainServicePhase::AwaitingGenesisTime;
        info!(target: LOG_TARGET, "entering {phase:?} phase");

        let mut ibd_skipped = false;
        loop {
            tokio::select! {
                () = genesis_timer.as_mut() => {
                    info!(target: LOG_TARGET, "genesis time reached: finishing {phase:?} phase");
                    return ibd_skipped;
                }
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query, phase).await;
                    }
                    ConsensusMsg::ApplyBlock { reply_channel, .. } => {
                        debug!(target: LOG_TARGET, "rejecting a block received during {phase:?} phase");
                        reply_channel.send(Err(Error::AwaitingGenesisTime)).unwrap_or_else(|_| {
                            error!(target: LOG_TARGET, "failed to send error through channel");
                        });
                    }
                    ConsensusMsg::ChainSync(event) => Self::reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        info!(target: LOG_TARGET, "Initial Block Download has been skipped during {phase:?} phase");
                        ibd_skipped = true;
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// `InitialBlockDownload` phase: Apply blocks until IBD completes.
    async fn phase_initial_block_download(&mut self) {
        let phase = ChainServicePhase::InitialBlockDownload;
        info!(target: LOG_TARGET, "entering {phase:?} phase");

        loop {
            tokio::select! {
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query, phase).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => Self::reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        info!(target: LOG_TARGET, "Initial Block Download completed");
                        return;
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// `ProlongedBootstrapPeriod` phase: Apply blocks until PBP is over.
    /// At the end of the phase, `Cryptarchia` is switched to online.
    #[expect(clippy::cognitive_complexity, reason = "better in one fn")]
    async fn phase_prolonged_bootstrap_period(mut self) -> Self {
        let phase = ChainServicePhase::ProlongedBootstrapPeriod;
        info!(target: LOG_TARGET, "entering {phase:?} phase");

        let mut pbp_timer = Box::pin(tokio::time::sleep(self.prolonged_bootstrap_period));
        loop {
            tokio::select! {
                () = pbp_timer.as_mut() => {
                    break;
                },
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query, phase).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => Self::reject_chain_sync_event(event).await,
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {phase:?} phase");
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }

        info!(target: LOG_TARGET, "Prolonged Bootstrap Period completed: switching chain to online");
        (self.cryptarchia, self.storage_blocks_to_remove) = Self::switch_to_online(
            self.cryptarchia,
            &self.storage_blocks_to_remove,
            self.relays.storage_adapter(),
            &self.chain_online_notifier,
        )
        .await;
        self.record_recovery_state();
        self
    }

    /// `Following` phase: Follow the chain in real time. This phase never ends.
    async fn phase_following(&mut self) {
        let phase = ChainServicePhase::Following;
        info!(target: LOG_TARGET, "entering {phase:?} phase");

        loop {
            tokio::select! {
                Some(msg) = self.inbound_relay.next() => match msg {
                    ConsensusMsg::Query(query) => {
                        self.process_query(query, phase).await;
                    }
                    ConsensusMsg::ApplyBlock { block, reply_channel } => {
                        self.apply_block_and_reply(*block, reply_channel).await;
                    }
                    ConsensusMsg::ChainSync(event) => {
                        Self::handle_chainsync_event(&self.cryptarchia, &self.sync_blocks_provider, event).await;
                    }
                    ConsensusMsg::IbdCompleted => {
                        debug!(target: LOG_TARGET, "ignoring IbdCompleted: already in {phase:?} phase");
                    }
                },
                Some(tick) = self.slot_timer.next() => self.current_slot = tick.slot,
                _ = self.state_recording_timer.tick() => self.record_recovery_state(),
            }
        }
    }

    /// Apply a block to the chain and reply with the result.
    async fn apply_block_and_reply(
        &mut self,
        block: Block<Tx>,
        reply_channel: oneshot::Sender<Result<(HeaderId, Vec<Tx>), Error>>,
    ) {
        match Self::process_block_and_update_state(
            &mut self.cryptarchia,
            block,
            self.current_slot,
            &self.storage_blocks_to_remove,
            &self.relays,
            &self.new_block_subscription_sender,
            &self.lib_subscription_sender,
            &self.state_updater,
        )
        .await
        {
            Ok((storage_blocks_to_remove, reorged_txs)) => {
                self.storage_blocks_to_remove = storage_blocks_to_remove;
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

    /// Serve a read-only query. Available in every phase.
    #[expect(clippy::too_many_lines, reason = "TODO: refactor into funcs")]
    async fn process_query(&self, query: Query, phase: ChainServicePhase) {
        match query {
            Query::Info { reply_channel } => {
                reply_channel
                    .send(ChainServiceInfo {
                        cryptarchia_info: self.cryptarchia.info(),
                        phase,
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

                let stream = Self::get_block_ids(
                    from_descendant,
                    to_ancestor,
                    &self.cryptarchia,
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

    /// Process a block and update the state accordingly.
    ///
    /// On error, the `cryptarchia` is not mutated.
    #[expect(clippy::too_many_arguments, reason = "Need all args")]
    async fn process_block_and_update_state(
        cryptarchia: &mut Cryptarchia,
        block: Block<Tx>,
        current_slot: Slot,
        storage_blocks_to_remove: &HashSet<HeaderId>,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
        lib_subscription_sender: &broadcast::Sender<LibUpdate>,
        state_updater: &StateUpdater<Option<CryptarchiaConsensusState>>,
    ) -> Result<(HashSet<HeaderId>, Vec<Tx>), Error> {
        let (pruned_blocks, reorged_txs) = Self::process_block(
            cryptarchia,
            block,
            current_slot,
            relays,
            new_block_subscription_sender,
            lib_subscription_sender,
        )
        .await?;

        let storage_blocks_to_remove = Self::delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            storage_blocks_to_remove,
            relays.storage_adapter(),
        )
        .await;

        persist_recovery_state(cryptarchia, storage_blocks_to_remove.clone(), state_updater);

        Ok((storage_blocks_to_remove, reorged_txs))
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
    async fn process_block(
        cryptarchia: &mut Cryptarchia,
        block: Block<Tx>,
        current_slot: Slot,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        new_block_subscription_sender: &broadcast::Sender<ProcessedBlockEvent>,
        lib_broadcaster: &broadcast::Sender<LibUpdate>,
    ) -> Result<(PrunedBlocks<HeaderId>, Vec<Tx>), Error> {
        debug!(target: LOG_TARGET, "Received proposal with ID: {:?}", block.header().id());
        let header = block.header();
        let prev_lib = cryptarchia.lib();

        let mut candidate = cryptarchia.clone();
        let (pruned_blocks, reorged_blocks, events) =
            candidate.try_apply_block(&block, current_slot)?;
        let new_lib = candidate.lib();

        let tx_count = block.transactions_iter().count();

        let immutable_blocks = Self::immutable_blocks_index(
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

    /// Store immutable block IDs to storage, including the new LIB if needed.
    /// If `prev_lib` is None, always includes the new LIB.
    /// If `prev_lib` is Some, only includes new LIB if it changed.
    async fn store_immutable_blocks_index(
        pruned_blocks: &PrunedBlocks<HeaderId>,
        prev_lib: Option<HeaderId>,
        new_lib: HeaderId,
        new_lib_slot: Slot,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<(), Error> {
        let immutable_blocks =
            Self::immutable_blocks_index(pruned_blocks, prev_lib, new_lib, new_lib_slot);

        storage_adapter
            .store_immutable_block_ids(immutable_blocks)
            .await
            .map_err(|e| Error::Storage(format!("Failed to store immutable block ids: {e}")))
    }

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

    /// Returns block IDs from descendant (inclusive) to ancestor
    /// (inclusive) in child-to-parent order.
    ///
    /// First tries to find blocks from memory. If any block is missing from
    /// memory, it falls back to loading all subsequent blocks from storage.
    fn get_block_ids(
        from_descendant: HeaderId,
        to_ancestor: HeaderId,
        cryptarchia: &Cryptarchia,
        storage_adapter: StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Pin<Box<dyn Stream<Item = Result<HeaderId, Error>> + Send>> {
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

        let storage_stream = stream::once(async move {
            Self::load_block_ids_from_storage(current, to_ancestor, storage_adapter)
        })
        .flatten();
        Box::pin(stream::iter(in_memory).chain(storage_stream))
    }

    /// Retrieves the block IDs from descendant (inclusive) to ancestor
    /// (inclusive) from the storage, in child-to-parent order.
    ///
    /// This is implemented here, and not as a method of `StorageAdapter`, to
    /// simplify the panic and error message handling.
    #[expect(closure_returning_async_block, reason = "required by try_unfold")]
    fn load_block_ids_from_storage(
        from_descendant: HeaderId,
        to_ancestor: HeaderId,
        storage: StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> impl Stream<Item = Result<HeaderId, Error>> {
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

    async fn load_recovery_blocks_from_storage(
        tip: HeaderId,
        lib: HeaderId,
        storage: StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<Vec<Block<Tx>>, Error> {
        let ids = Self::load_block_ids_from_storage(tip, lib, storage.clone())
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
    async fn initialize_cryptarchia<TimeBackend>(
        service: &CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>,
        bootstrap_config: &BootstrapConfig,
        ledger_config: lb_ledger::Config,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        current_slot: Slot,
    ) -> InitializedCryptarchia
    where
        TimeBackend: lb_time_service::backends::TimeBackend,
    {
        info!(
            target: LOG_TARGET, tip = ?service.state.tip, lib = ?service.state.lib, lib_height = service.state.lib_block_length, genesis = ?service.state.genesis_id,
            "recovering chain state",
        );

        let lib_id = service.state.lib;
        let genesis_id = service.state.genesis_id;
        let state = choose_engine_state(
            lib_id,
            genesis_id,
            bootstrap_config,
            service.state.last_engine_state.as_ref(),
        );
        let mut cryptarchia = Cryptarchia::from_lib(
            lib_id,
            service.state.lib_ledger_state.clone(),
            genesis_id,
            ledger_config,
            state,
            service.state.lib_block_slot,
            service.state.lib_block_length,
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
        if let Err(e) = service.new_block_subscription_sender.send(init_event) {
            debug!("No new-block subscribers to notify: {e}");
        }

        // Phase 1: Collect and load blocks in (LIB, tip].
        info!(
            target: LOG_TARGET, lib = ?lib_id, tip = ?service.state.tip,
            "loading stored blocks for chain recovery",
        );
        let RecoveryBlocks {
            blocks,
            fell_back_to_lib,
        } = Self::load_recovery_blocks_or_fall_back_to_lib(
            service.state.tip,
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
            match Self::process_block(
                &mut cryptarchia,
                block,
                current_slot,
                relays,
                &service.new_block_subscription_sender,
                &service.lib_subscription_sender,
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

    /// Remove the stale blocks from the storage layer.
    ///
    /// Also, this removes the `additional_blocks` from the storage
    /// layer. These blocks might belong to previous pruning operations and
    /// that failed to be removed from the storage for some reason.
    ///
    /// This function returns any block that fails to be deleted from the
    /// storage layer.
    async fn delete_stale_blocks_from_storage(
        stale_blocks: impl Iterator<Item = HeaderId> + Send,
        additional_blocks: &HashSet<HeaderId>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> HashSet<HeaderId> {
        match Self::delete_blocks_from_storage(
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
    async fn delete_blocks_from_storage<Headers>(
        block_headers: Headers,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Result<(), Vec<(HeaderId, DynError)>>
    where
        Headers: Iterator<Item = HeaderId> + Send,
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

    async fn handle_chainsync_event(
        cryptarchia: &Cryptarchia,
        sync_blocks_provider: &BlockProvider<Storage, Tx>,
        event: ChainSyncEvent,
    ) {
        match event {
            ChainSyncEvent::ProvideBlocksRequest {
                target_block,
                local_tip,
                latest_immutable_block,
                additional_blocks,
                reply_sender,
            } => {
                let known_blocks = vec![local_tip, latest_immutable_block]
                    .into_iter()
                    .chain(additional_blocks)
                    .collect::<HashSet<_>>();

                sync_blocks_provider
                    .send_blocks(
                        &cryptarchia.consensus,
                        target_block,
                        &known_blocks,
                        reply_sender,
                    )
                    .await;
            }
            ChainSyncEvent::ProvideTipRequest { reply_sender } => {
                let tip = cryptarchia.consensus.tip_branch();
                let response = ProviderResponse::Available(GetTipResponse::Tip {
                    tip: tip.id(),
                    slot: tip.slot(),
                    height: tip.length(),
                });

                trace!("Sending tip response: {response:?}");
                if let Err(e) = reply_sender.send(response).await {
                    error!("Failed to send tip header: {e}");
                }
            }
        }
    }

    async fn reject_chain_sync_event(event: ChainSyncEvent) {
        debug!(target: LOG_TARGET, "Received chainsync event while in bootstrapping state. Ignoring it.");
        match event {
            ChainSyncEvent::ProvideBlocksRequest { reply_sender, .. } => {
                let response = ProviderResponse::Unavailable {
                    reason: BlocksUnavailableReason::Unknown(
                        "Node is not in online mode".to_owned(),
                    ),
                };
                if let Err(e) = reply_sender.send(response).await {
                    error!("Failed to send chain sync response: {e}");
                }
            }
            ChainSyncEvent::ProvideTipRequest { reply_sender } => {
                Self::send_chain_sync_rejection(reply_sender).await;
            }
        }
    }

    async fn send_chain_sync_rejection<ResponseType>(
        sender: mpsc::Sender<ProviderResponse<ResponseType>>,
    ) {
        let response = ProviderResponse::Unavailable {
            reason: "Node is not in online mode".to_owned(),
        };
        if let Err(e) = sender.send(response).await {
            error!("Failed to send chain sync response: {e}");
        }
    }

    async fn switch_to_online(
        cryptarchia: Cryptarchia,
        storage_blocks_to_remove: &HashSet<HeaderId>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
        chain_online_notifier: &ChainOnlineNotifier,
    ) -> (Cryptarchia, HashSet<HeaderId>) {
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        info!("Node is online and following the chain");

        chain_online_notifier.notify();

        if let Err(e) = Self::store_immutable_blocks_index(
            &pruned_blocks,
            None,
            cryptarchia.lib(),
            cryptarchia.consensus.lib_branch().slot(),
            storage_adapter,
        )
        .await
        {
            error!("Could not store immutable block IDs: {e}");
        }

        let storage_blocks_to_remove = Self::delete_stale_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            storage_blocks_to_remove,
            storage_adapter,
        )
        .await;

        (cryptarchia, storage_blocks_to_remove)
    }
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
fn persist_recovery_state(
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
