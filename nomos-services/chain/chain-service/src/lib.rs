pub mod api;
mod bootstrap;
mod relays;
mod states;
pub mod storage;
mod sync;

use core::fmt::Debug;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Display,
    path::PathBuf,
    time::Duration,
};

use broadcast_service::{BlockBroadcastMsg, BlockBroadcastService, BlockInfo, SessionUpdate};
use bytes::Bytes;
use cryptarchia_engine::PrunedBlocks;
pub use cryptarchia_engine::{Epoch, Slot};
use cryptarchia_sync::{GetTipResponse, ProviderResponse};
use futures::{FutureExt as _, StreamExt as _};
use nomos_core::{
    block::Block,
    header::HeaderId,
    mantle::{
        AuthenticatedMantleTx, Transaction, TxHash, gas::MainnetGasConstants,
        genesis_tx::GenesisTx, ops::leader_claim::VoucherCm,
    },
    sdp::{Declaration, DeclarationId, ProviderId, ProviderInfo, ServiceType},
};
pub use nomos_ledger::EpochState;
use nomos_ledger::LedgerState;
use nomos_network::message::ChainSyncEvent;
use nomos_storage::{StorageService, api::chain::StorageChainApi, backends::StorageBackend};
use nomos_time::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{AsServiceId, ServiceCore, ServiceData, state::StateUpdater},
};
use relays::BroadcastRelay;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_with::serde_as;
use services_utils::{
    overwatch::{JsonFileBackend, RecoveryOperator, recovery::backends::FileBackendSettings},
    wait_until_services_are_ready,
};
use strum::IntoEnumIterator as _;
use thiserror::Error;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::Instant,
};
use tracing::{Level, debug, error, info, instrument, span, warn};
use tracing_futures::Instrument as _;

pub use crate::bootstrap::config::{BootstrapConfig, OfflineGracePeriodConfig};
use crate::{
    bootstrap::state::choose_engine_state,
    relays::CryptarchiaConsensusRelays,
    states::CryptarchiaConsensusState,
    storage::{StorageAdapter as _, adapters::StorageAdapter},
    sync::block_provider::BlockProvider,
};

// Limit the number of blocks returned by GetHeaders
const HEADERS_LIMIT: usize = 512;
const CRYPTARCHIA_ID: &str = "Cryptarchia";

pub(crate) const LOG_TARGET: &str = "cryptarchia::service";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Missing parent while applying block {parent}, {info:?}")]
    ParentMissing {
        parent: HeaderId,
        info: CryptarchiaInfo,
    },
    #[error("Block from future slot({block_slot:?}): current_slot:{current_slot:?}")]
    FutureBlock {
        block_slot: Slot,
        current_slot: Slot,
    },
    #[error("Ledger error: {0}")]
    Ledger(#[from] nomos_ledger::LedgerError<HeaderId>),
    #[error("Consensus error: {0}")]
    Consensus(#[from] cryptarchia_engine::Error<HeaderId>),
    #[error("Serialization error: {0}")]
    Serialisation(#[from] nomos_core::codec::Error),
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Storage error: {0}")]
    Storage(String),
    #[error("Mempool error: {0}")]
    Mempool(String),
    #[error("Block header id not found: {0}")]
    HeaderIdNotFound(HeaderId),
    #[error("Service session not found: {0:?}")]
    ServiceSessionNotFound(ServiceType),
}

#[derive(Debug)]
pub enum ConsensusMsg<Tx> {
    Info {
        tx: oneshot::Sender<CryptarchiaInfo>,
    },
    NewBlockSubscribe {
        sender: oneshot::Sender<broadcast::Receiver<HeaderId>>,
    },
    LibSubscribe {
        sender: oneshot::Sender<broadcast::Receiver<LibUpdate>>,
    },
    GetHeaders {
        from: Option<HeaderId>,
        to: Option<HeaderId>,
        tx: oneshot::Sender<Vec<HeaderId>>,
    },
    GetLedgerState {
        block_id: HeaderId,
        tx: oneshot::Sender<Option<LedgerState>>,
    },
    GetSdpDeclarations {
        tx: oneshot::Sender<Vec<(DeclarationId, Declaration)>>,
    },
    GetEpochState {
        slot: Slot,
        tx: oneshot::Sender<Option<EpochState>>,
    },
    ApplyBlock {
        block: Box<Block<Tx>>,
        tx: oneshot::Sender<Result<(), Error>>,
    },
    /// Forward chain sync events from the network to chain-service.
    /// Chain-service will handle these directly and respond via the embedded
    /// `reply_sender`.
    ChainSync(ChainSyncEvent),
    /// Notification from chain-network that Initial Block Download has
    /// completed. Chain-service should start the prolonged bootstrap timer
    /// upon receiving this.
    IbdCompleted,
}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CryptarchiaInfo {
    pub lib: HeaderId,
    pub tip: HeaderId,
    pub slot: Slot,
    pub height: u64,
    pub mode: cryptarchia_engine::State,
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

impl PrunedBlocksInfo {
    /// Returns an iterator over all pruned blocks, both stale and immutable.
    pub fn all(&self) -> impl Iterator<Item = HeaderId> + '_ {
        self.stale_blocks
            .iter()
            .chain(self.immutable_blocks.values())
            .copied()
    }
}

#[derive(Clone)]
pub struct Cryptarchia {
    pub ledger: nomos_ledger::Ledger<HeaderId>,
    pub consensus: cryptarchia_engine::Cryptarchia<HeaderId>,
    pub genesis_id: HeaderId,
}

impl Cryptarchia {
    /// Initialize a new [`Cryptarchia`] instance.
    #[must_use]
    pub fn from_lib(
        lib_id: HeaderId,
        lib_ledger_state: LedgerState,
        genesis_id: HeaderId,
        ledger_config: nomos_ledger::Config,
        state: cryptarchia_engine::State,
    ) -> Self {
        Self {
            consensus: <cryptarchia_engine::Cryptarchia<_>>::from_lib(
                lib_id,
                ledger_config.consensus_config,
                state,
            ),
            ledger: <nomos_ledger::Ledger<_>>::new(lib_id, lib_ledger_state, ledger_config),
            genesis_id,
        }
    }

    #[must_use]
    pub fn info(&self) -> CryptarchiaInfo {
        let tip_branch = self
            .consensus
            .branches()
            .get(&self.tip())
            .expect("tip branch not available");

        CryptarchiaInfo {
            lib: self.lib(),
            tip: self.tip(),
            slot: tip_branch.slot(),
            height: tip_branch.length(),
            mode: *self.consensus.state(),
        }
    }

    #[must_use]
    pub const fn tip(&self) -> HeaderId {
        self.consensus.tip()
    }

    #[must_use]
    pub const fn lib(&self) -> HeaderId {
        self.consensus.lib()
    }

    /// Create a new [`Cryptarchia`] with the updated state.
    #[must_use = "Returns a new instance with the updated state, without modifying the original."]
    fn try_apply_block<Tx>(
        &self,
        block: &Block<Tx>,
        current_slot: Slot,
    ) -> Result<(Self, PrunedBlocks<HeaderId>), Error>
    where
        Tx: AuthenticatedMantleTx,
    {
        let header = block.header();
        let id = header.id();
        let parent = header.parent();
        let slot = header.slot();

        // Reject blocks from future slots
        if slot > current_slot {
            return Err(Error::FutureBlock {
                block_slot: slot,
                current_slot,
            });
        }

        // A block number of this block if it's applied to the chain.
        let ledger = self
            .ledger
            .try_update::<_, MainnetGasConstants>(
                id,
                parent,
                slot,
                header.leader_proof(),
                VoucherCm::default(), // TODO: add the new voucher commitment here
                block.transactions(),
            )
            .map_err(|err| match err {
                nomos_ledger::LedgerError::ParentNotFound(parent) => Error::ParentMissing {
                    parent,
                    info: self.info(),
                },
                err => Error::Ledger(err),
            })?;
        let (consensus, pruned_blocks) =
            self.consensus
                .receive_block(id, parent, slot)
                .map_err(|err| match err {
                    cryptarchia_engine::Error::ParentMissing(parent) => Error::ParentMissing {
                        parent,
                        info: self.info(),
                    },
                    err => Error::Consensus(err),
                })?;

        let mut cryptarchia = Self {
            ledger,
            consensus,
            genesis_id: self.genesis_id,
        };
        // Prune the ledger states of all the pruned blocks.
        cryptarchia.prune_ledger_states(pruned_blocks.all());

        Ok((cryptarchia, pruned_blocks))
    }

    fn epoch_state_for_slot(&self, slot: Slot) -> Option<&EpochState> {
        let tip = self.tip();
        let state = self.ledger.state(&tip).expect("no state for tip");
        let requested_epoch = self.ledger.config().epoch(slot);
        if state.epoch_state().epoch() == requested_epoch {
            Some(state.epoch_state())
        } else if requested_epoch == state.next_epoch_state().epoch() {
            Some(state.next_epoch_state())
        } else {
            None
        }
    }

    /// Remove the ledger states associated with blocks that have been pruned by
    /// the [`cryptarchia_engine::Cryptarchia`].
    ///
    /// Details on which blocks are pruned can be found in the
    /// [`cryptarchia_engine::Cryptarchia::receive_block`].
    fn prune_ledger_states<'a>(&'a mut self, blocks: impl Iterator<Item = &'a HeaderId>) {
        let mut pruned_states_count = 0usize;
        for block in blocks {
            if self.ledger.prune_state_at(block) {
                pruned_states_count = pruned_states_count.saturating_add(1);
            } else {
                tracing::error!(
                   target: LOG_TARGET,
                    "Failed to prune ledger state for block {:?} which should exist.",
                    block
                );
            }
        }
        tracing::debug!(target: LOG_TARGET, "Pruned {pruned_states_count} old forks and their ledger states.");
    }

    fn online(self) -> (Self, PrunedBlocks<HeaderId>) {
        let (consensus, pruned_blocks) = self.consensus.online();
        (
            Self {
                ledger: self.ledger,
                consensus,
                genesis_id: self.genesis_id,
            },
            pruned_blocks,
        )
    }

    const fn is_boostrapping(&self) -> bool {
        self.consensus.state().is_bootstrapping()
    }

    const fn state(&self) -> &cryptarchia_engine::State {
        self.consensus.state()
    }

    #[must_use]
    pub fn has_block(&self, block_id: &HeaderId) -> bool {
        self.consensus.branches().get(block_id).is_some()
    }

    fn active_session_providers(
        &self,
        block_id: &HeaderId,
        service_type: ServiceType,
    ) -> Result<HashMap<ProviderId, ProviderInfo>, Error> {
        let ledger = self
            .ledger
            .state(block_id)
            .ok_or(Error::HeaderIdNotFound(*block_id))?;

        ledger
            .active_session_providers(service_type)
            .ok_or(Error::ServiceSessionNotFound(service_type))
    }

    fn active_sessions_numbers(
        &self,
        block_id: &HeaderId,
    ) -> Result<HashMap<ServiceType, u64>, Error> {
        let ledger = self
            .ledger
            .state(block_id)
            .ok_or(Error::HeaderIdNotFound(*block_id))?;

        Ok(ledger.active_sessions())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CryptarchiaSettings {
    pub config: nomos_ledger::Config,
    pub starting_state: StartingState,
    pub recovery_file: PathBuf,
    pub bootstrap: BootstrapConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum StartingState {
    Genesis {
        genesis_tx: GenesisTx,
    },
    Lib {
        lib_id: HeaderId,
        lib_ledger_state: Box<LedgerState>,
        genesis_id: HeaderId,
    },
}

impl FileBackendSettings for CryptarchiaSettings {
    fn recovery_file(&self) -> &PathBuf {
        &self.recovery_file
    }
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: AuthenticatedMantleTx + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    TimeBackend: nomos_time::backends::TimeBackend,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    new_block_subscription_sender: broadcast::Sender<HeaderId>,
    lib_subscription_sender: broadcast::Sender<LibUpdate>,
    state: <Self as ServiceData>::State,
}

impl<Tx, Storage, TimeBackend, RuntimeServiceId> ServiceData
    for CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: AuthenticatedMantleTx + Clone + Eq + Debug,
    Storage: StorageBackend + Send + Sync + 'static,
    <Storage as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    TimeBackend: nomos_time::backends::TimeBackend,
{
    type Settings = CryptarchiaSettings;
    type State = CryptarchiaConsensusState;
    type StateOperator = RecoveryOperator<JsonFileBackend<Self::State, Self::Settings>>;
    type Message = ConsensusMsg<Tx>;
}

#[async_trait::async_trait]
impl<Tx, Storage, TimeBackend, RuntimeServiceId> ServiceCore<RuntimeServiceId>
    for CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: Transaction<Hash = TxHash>
        + AuthenticatedMantleTx
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
    TimeBackend: nomos_time::backends::TimeBackend,
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

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), DynError> {
        let relays: CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId> =
            CryptarchiaConsensusRelays::from_service_resources_handle::<TimeBackend>(
                &self.service_resources_handle,
            )
            .await;

        let CryptarchiaSettings {
            config: ledger_config,
            bootstrap: bootstrap_config,
            ..
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            BlockBroadcastService<_>,
            StorageService<_, _>,
            TimeService<_, _>
        )
        .await?;

        let (mut current_slot, mut slot_timer) = Self::get_slot_timer(&relays).await?;

        // TODO: check active slot coeff is exactly 1/30
        let (mut cryptarchia, pruned_blocks) = self
            .initialize_cryptarchia(
                &bootstrap_config,
                ledger_config.clone(),
                &relays,
                current_slot,
            )
            .await;
        // These are blocks that have been pruned by the cryptarchia engine but have not
        // yet been deleted from the storage layer.
        let mut storage_blocks_to_remove = Self::delete_pruned_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            &self.state.storage_blocks_to_remove,
            relays.storage_adapter(),
        )
        .await;

        let sync_blocks_provider: BlockProvider<_, _> =
            BlockProvider::new(relays.storage_adapter().storage_relay.clone());

        // The prolonged bootstrap timer will be started when chain-network notifies us
        // that IBD has completed. This ensures we don't transition to Online mode
        // before the node has caught up with the network.
        let mut prolonged_bootstrap_timer: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

        // Start the timer for periodic state recording for offline grace period
        let mut state_recording_timer = tokio::time::interval(
            bootstrap_config
                .offline_grace_period
                .state_recording_interval,
        );

        // Mark the service as ready. The service is operational and can handle requests
        // even while in bootstrap mode waiting for IBD to complete.
        self.notify_service_ready();

        let async_loop = async {
            loop {
                tokio::select! {
                    () = async { prolonged_bootstrap_timer.as_mut().unwrap().as_mut().await }, if prolonged_bootstrap_timer.is_some() && cryptarchia.is_boostrapping() => {
                        info!("Prolonged Bootstrap Period has passed. Switching to Online.");
                        (cryptarchia, storage_blocks_to_remove) = Self::switch_to_online(
                            cryptarchia,
                            &storage_blocks_to_remove,
                            relays.storage_adapter(),
                        ).await;
                        Self::update_state(
                            &cryptarchia,
                            storage_blocks_to_remove.clone(),
                            &self.service_resources_handle.state_updater,
                        );
                    }

                    Some(msg) = self.service_resources_handle.inbound_relay.next() => {
                        // Handle ApplyBlock, ChainSync, and IbdCompleted separately since they need async context
                        match msg {
                            ConsensusMsg::IbdCompleted => {
                                info!("Received IBD completion notification. Starting prolonged bootstrap timer.");
                                // Start the prolonged bootstrap timer now that IBD is complete
                                prolonged_bootstrap_timer = Some(Box::pin(tokio::time::sleep_until(
                                    Instant::now() + bootstrap_config.prolonged_bootstrap_period,
                                )));
                            }
                            ConsensusMsg::ApplyBlock { block, tx } => {
                                // TODO: move this into the process_message() function after making the process_message async.
                                match Self::process_block_and_update_state(
                                        cryptarchia.clone(),
                                        *block,
                                        current_slot,
                                        &storage_blocks_to_remove,
                                        &relays,
                                        &self.new_block_subscription_sender,
                                        &self.lib_subscription_sender,
                                        &self.service_resources_handle.state_updater,
                                    ).await {
                                    Ok((new_cryptarchia, new_storage_blocks_to_remove)) => {
                                        cryptarchia = new_cryptarchia;
                                        storage_blocks_to_remove = new_storage_blocks_to_remove;
                                        tx.send(Ok(())).unwrap_or_else(|_| {
                                            error!("Could not send process block result through channel");
                                        });
                                    }
                                    Err(e) => {
                                        let error_msg = format!("Failed to process block: {e:?}");
                                        error!(target: LOG_TARGET, "{}", error_msg);
                                        tx.send(Err(e)).unwrap_or_else(|_| {
                                            error!("Could not send process block error through channel");
                                        });
                                    }
                                }
                            }
                            ConsensusMsg::ChainSync(event) => {
                                if cryptarchia.state().is_online() {
                                    Self::handle_chainsync_event(&cryptarchia, &sync_blocks_provider, event).await;
                                } else {
                                    Self::reject_chain_sync_event(event).await;
                                }
                            }
                            msg => {
                                Self::process_message(&cryptarchia, &self.new_block_subscription_sender, &self.lib_subscription_sender, msg);
                            }
                        }
                    }

                    Some(nomos_time::SlotTick { slot, .. }) = slot_timer.next() => {
                        current_slot = slot;
                    }

                    _ = state_recording_timer.tick() => {
                        // Periodically record the current timestamp and engine state
                        Self::update_state(
                            &cryptarchia,
                            storage_blocks_to_remove.clone(),
                            &self.service_resources_handle.state_updater,
                        );
                    }
                }
            }
        };

        // It sucks to use `CRYPTARCHIA_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        async_loop
            .instrument(span!(Level::TRACE, CRYPTARCHIA_ID))
            .await;

        Ok(())
    }
}

impl<Tx, Storage, TimeBackend, RuntimeServiceId>
    CryptarchiaConsensus<Tx, Storage, TimeBackend, RuntimeServiceId>
where
    Tx: Transaction<Hash = TxHash>
        + AuthenticatedMantleTx
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
    TimeBackend: nomos_time::backends::TimeBackend,
    RuntimeServiceId: Display + AsServiceId<Self>,
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
    ) -> Result<(Slot, nomos_time::EpochSlotTickStream), DynError> {
        let slot_timer = {
            let (sender, receiver) = oneshot::channel();
            relays
                .time_relay()
                .send(nomos_time::TimeServiceMessage::Subscribe { sender })
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
                .send(nomos_time::TimeServiceMessage::CurrentSlot { sender })
                .await
                .expect("Request current slot from time service should succeed");
            receiver.await?.slot
        };

        Ok((current_slot, slot_timer))
    }

    fn process_message(
        cryptarchia: &Cryptarchia,
        new_block_channel: &broadcast::Sender<HeaderId>,
        lib_channel: &broadcast::Sender<LibUpdate>,
        msg: ConsensusMsg<Tx>,
    ) {
        match msg {
            ConsensusMsg::Info { tx } => {
                tx.send(cryptarchia.info()).unwrap_or_else(|e| {
                    error!("Could not send consensus info through channel: {:?}", e);
                });
            }
            ConsensusMsg::NewBlockSubscribe { sender } => {
                sender
                    .send(new_block_channel.subscribe())
                    .unwrap_or_else(|_| {
                        error!("Could not subscribe to new block channel");
                    });
            }
            ConsensusMsg::LibSubscribe { sender } => {
                sender.send(lib_channel.subscribe()).unwrap_or_else(|_| {
                    error!("Could not subscribe to LIB updates channel");
                });
            }
            ConsensusMsg::GetHeaders { from, to, tx } => {
                // default to tip block if not present
                let from = from.unwrap_or_else(|| cryptarchia.tip());
                // default to LIB block if not present
                // TODO: for a full history, we should use genesis, but we don't want to
                // keep it all in memory, headers past LIB should be fetched from storage
                let to = to.unwrap_or_else(|| cryptarchia.lib());

                let mut res = Vec::new();
                let mut cur = from;

                let branches = cryptarchia.consensus.branches();
                while let Some(h) = branches.get(&cur) {
                    res.push(h.id());
                    // limit the response size
                    if cur == to || cur == cryptarchia.lib() || res.len() >= HEADERS_LIMIT {
                        break;
                    }
                    cur = h.parent();
                }

                tx.send(res)
                    .unwrap_or_else(|_| error!("could not send blocks through channel"));
            }
            ConsensusMsg::GetLedgerState { block_id, tx } => {
                let ledger_state = cryptarchia.ledger.state(&block_id).cloned();
                tx.send(ledger_state).unwrap_or_else(|_| {
                    error!("Could not send ledger state through channel");
                });
            }
            ConsensusMsg::GetSdpDeclarations { tx } => {
                let tip = cryptarchia.tip();
                let declarations = cryptarchia
                    .ledger
                    .state(&tip)
                    .map(LedgerState::sdp_declarations)
                    .unwrap_or_default();

                tx.send(declarations).unwrap_or_else(|_| {
                    error!("Could not send SDP declarations through channel");
                });
            }
            ConsensusMsg::GetEpochState { slot, tx } => {
                let epoch_state = cryptarchia.epoch_state_for_slot(slot).cloned();
                tx.send(epoch_state).unwrap_or_else(|_| {
                    error!("Could not send epoch state through channel");
                });
            }
            ConsensusMsg::ApplyBlock { .. } => {
                // ApplyBlock is handled separately in the run loop where we have async
                // context This should never be reached since we filter it out
                // before calling process_message
                panic!("ApplyBlock should be handled in the run loop, not in process_message");
            }
            ConsensusMsg::ChainSync(_) => {
                // ChainSync is handled separately in the run loop where we have async
                // context. This should never be reached since we filter it out
                // before calling process_message
                panic!("ChainSync should be handled in the run loop, not in process_message");
            }
            ConsensusMsg::IbdCompleted => {
                // IbdCompleted is handled separately in the run loop where we need to modify
                // the prolonged_bootstrap_timer. This should never be reached since we filter
                // it out before calling process_message
                panic!("IbdCompleted should be handled in the run loop, not in process_message");
            }
        }
    }

    #[expect(clippy::too_many_arguments, reason = "Need all args")]
    async fn process_block_and_update_state(
        cryptarchia: Cryptarchia,
        block: Block<Tx>,
        current_slot: Slot,
        storage_blocks_to_remove: &HashSet<HeaderId>,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        new_block_subscription_sender: &broadcast::Sender<HeaderId>,
        lib_subscription_sender: &broadcast::Sender<LibUpdate>,
        state_updater: &StateUpdater<Option<CryptarchiaConsensusState>>,
    ) -> Result<(Cryptarchia, HashSet<HeaderId>), Error> {
        let (cryptarchia, pruned_blocks) = Self::process_block(
            cryptarchia,
            block,
            current_slot,
            relays,
            new_block_subscription_sender,
            lib_subscription_sender,
        )
        .await?;

        let storage_blocks_to_remove = Self::delete_pruned_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            storage_blocks_to_remove,
            relays.storage_adapter(),
        )
        .await;

        Self::update_state(
            &cryptarchia,
            storage_blocks_to_remove.clone(),
            state_updater,
        );

        Ok((cryptarchia, storage_blocks_to_remove))
    }

    fn update_state(
        cryptarchia: &Cryptarchia,
        storage_blocks_to_remove: HashSet<HeaderId>,
        state_updater: &StateUpdater<Option<CryptarchiaConsensusState>>,
    ) {
        match <Self as ServiceData>::State::from_cryptarchia_and_unpruned_blocks(
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

    /// Try to add a [`Block`] to [`Cryptarchia`].
    /// A [`Block`] is only added if it's valid
    #[expect(clippy::allow_attributes_without_reason)]
    #[instrument(level = "debug", skip(cryptarchia, relays))]
    async fn process_block(
        cryptarchia: Cryptarchia,
        block: Block<Tx>,
        current_slot: Slot,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        new_block_subscription_sender: &broadcast::Sender<HeaderId>,
        lib_broadcaster: &broadcast::Sender<LibUpdate>,
    ) -> Result<(Cryptarchia, PrunedBlocks<HeaderId>), Error> {
        debug!("received proposal {:?}", block);

        let header = block.header();
        let prev_lib = cryptarchia.lib();

        let previous_session_numbers = match cryptarchia.active_sessions_numbers(&prev_lib) {
            Ok(session_numbers) => session_numbers,
            Err(e) => {
                warn!("Error getting previous session numbers: {e}");
                ServiceType::iter().map(|s| (s, 0)).collect()
            }
        };

        let (cryptarchia, pruned_blocks) = cryptarchia.try_apply_block(&block, current_slot)?;
        let new_lib = cryptarchia.lib();

        relays
            .storage_adapter()
            .store_block(header.id(), block.clone())
            .await
            .map_err(|e| Error::Storage(format!("Failed to store block: {e}")))?;

        let immutable_blocks = pruned_blocks.immutable_blocks().clone();
        relays
            .storage_adapter()
            .store_immutable_block_ids(immutable_blocks)
            .await
            .map_err(|e| Error::Storage(format!("Failed to store immutable block ids: {e}")))?;

        if let Err(e) = new_block_subscription_sender.send(header.id()) {
            error!("Could not notify new block to services {e}");
        }

        if prev_lib != new_lib {
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
                error!("Could not notify block to services {e}");
            }

            let lib_update = LibUpdate {
                new_lib: cryptarchia.lib(),
                pruned_blocks: PrunedBlocksInfo {
                    stale_blocks: pruned_blocks.stale_blocks().copied().collect(),
                    immutable_blocks: pruned_blocks.immutable_blocks().clone(),
                },
            };

            if let Err(e) = lib_broadcaster.send(lib_update) {
                error!("Could not notify LIB update to services: {e}");
            }

            Self::broadcast_session_updates_for_block(
                &cryptarchia,
                &new_lib,
                relays,
                Some(&previous_session_numbers),
            )
            .await;
        }

        Ok((cryptarchia, pruned_blocks))
    }

    /// Retrieves the blocks in the range from `from` to `to` from the storage.
    /// Both `from` and `to` are included in the range.
    /// This is implemented here, and not as a method of `StorageAdapter`, to
    /// simplify the panic and error message handling.
    ///
    /// # Panics
    ///
    /// Panics if any of the blocks in the range are not found in the storage.
    ///
    /// # Parameters
    ///
    /// * `from` - The header id of the first block in the range. Must be a
    ///   valid header.
    /// * `to` - The header id of the last block in the range. Must be a valid
    ///   header.
    ///
    /// # Returns
    ///
    /// A vector of blocks in the range from `from` to `to`.
    /// If no blocks are found, returns an empty vector.
    /// If any of the [`HeaderId`]s are invalid, returns an error with the first
    /// invalid header id.
    async fn get_blocks_in_range(
        from: HeaderId,
        to: HeaderId,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> Vec<Block<Tx>> {
        // Due to the blocks traversal order, this yields `to..from` order
        let blocks = futures::stream::unfold(to, async |header_id| {
            if header_id == from {
                None
            } else {
                let block = storage_adapter
                    .get_block(&header_id)
                    .await
                    .unwrap_or_else(|| {
                        panic!("Could not retrieve block {to} from storage during recovery")
                    });
                let parent_header_id = block.header().parent();
                Some((block, parent_header_id))
            }
        });

        // To avoid confusion, the order is reversed so it fits the natural `from..to`
        // order
        blocks.collect::<Vec<_>>().await.into_iter().rev().collect()
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
    async fn initialize_cryptarchia(
        &self,
        bootstrap_config: &BootstrapConfig,
        ledger_config: nomos_ledger::Config,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        current_slot: Slot,
    ) -> (Cryptarchia, PrunedBlocks<HeaderId>) {
        let lib_id = self.state.lib;
        let genesis_id = self.state.genesis_id;
        let state = choose_engine_state(
            lib_id,
            genesis_id,
            bootstrap_config,
            self.state.last_engine_state.as_ref(),
        );
        let mut cryptarchia = Cryptarchia::from_lib(
            lib_id,
            self.state.lib_ledger_state.clone(),
            genesis_id,
            ledger_config,
            state,
        );

        // We reapply blocks here instead of saving ledger states to correcly make use
        // of structural sharing If forking is low, this might not be necessary
        let blocks =
            Self::get_blocks_in_range(lib_id, self.state.tip, relays.storage_adapter()).await;

        // Skip LIB block since it's already applied
        let blocks = blocks.into_iter().skip(1);

        // Stream the already applied state.
        let init_tip = cryptarchia.tip();
        if let Err(e) = self.new_block_subscription_sender.send(init_tip) {
            error!("Could not notify new block to services {e}");
        }
        Self::broadcast_session_updates_for_block(&cryptarchia, &init_tip, relays, None).await;

        let mut pruned_blocks = PrunedBlocks::new();
        for block in blocks {
            match Self::process_block(
                cryptarchia.clone(),
                block,
                current_slot,
                relays,
                &self.new_block_subscription_sender,
                &self.lib_subscription_sender,
            )
            .await
            {
                Ok((new_cryptarchia, new_pruned_blocks)) => {
                    cryptarchia = new_cryptarchia;
                    pruned_blocks.extend(&new_pruned_blocks);
                }
                Err(e) => {
                    error!(target: LOG_TARGET, "Error processing block: {:?}", e);
                }
            }
        }

        (cryptarchia, pruned_blocks)
    }

    /// Remove the pruned blocks from the storage layer.
    ///
    /// Also, this removes the `additional_blocks` from the storage
    /// layer. These blocks might belong to previous pruning operations and
    /// that failed to be removed from the storage for some reason.
    ///
    /// This function returns any block that fails to be deleted from the
    /// storage layer.
    async fn delete_pruned_blocks_from_storage(
        pruned_blocks: impl Iterator<Item = HeaderId> + Send,
        additional_blocks: &HashSet<HeaderId>,
        storage_adapter: &StorageAdapter<Storage, Tx, RuntimeServiceId>,
    ) -> HashSet<HeaderId> {
        match Self::delete_blocks_from_storage(
            pruned_blocks.chain(additional_blocks.iter().copied()),
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
                    tracing::debug!(
                        target: LOG_TARGET,
                        "Block {block_id:#?} successfully deleted from storage."
                    );
                    None
                }
                Ok(None) => {
                    tracing::trace!(
                        target: LOG_TARGET,
                        "Block {block_id:#?} was not found in storage."
                    );
                    None
                }
                Err(e) => {
                    tracing::error!(
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
                    .chain(additional_blocks.into_iter())
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

                info!("Sending tip response: {response:?}");
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
                Self::send_chain_sync_rejection(reply_sender).await;
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
    ) -> (Cryptarchia, HashSet<HeaderId>) {
        let (cryptarchia, pruned_blocks) = cryptarchia.online();
        if let Err(e) = storage_adapter
            .store_immutable_block_ids(pruned_blocks.immutable_blocks().clone())
            .await
        {
            error!("Could not store immutable block IDs: {e}");
        }

        let storage_blocks_to_remove = Self::delete_pruned_blocks_from_storage(
            pruned_blocks.stale_blocks().copied(),
            storage_blocks_to_remove,
            storage_adapter,
        )
        .await;

        (cryptarchia, storage_blocks_to_remove)
    }

    async fn broadcast_session_updates_for_block(
        cryptarchia: &Cryptarchia,
        block_id: &HeaderId,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        previous_sessions: Option<&HashMap<ServiceType, u64>>,
    ) {
        let Ok(new_sessions) = cryptarchia.active_sessions_numbers(block_id) else {
            error!("Could not get active session numbers for block {block_id:?}");
            return;
        };

        for (service, new_session_number) in &new_sessions {
            Self::handle_service_update(
                cryptarchia,
                block_id,
                relays,
                previous_sessions,
                service,
                new_session_number,
            )
            .await;
        }
    }

    async fn handle_service_update(
        cryptarchia: &Cryptarchia,
        block_id: &HeaderId,
        relays: &CryptarchiaConsensusRelays<Tx, Storage, RuntimeServiceId>,
        previous_sessions: Option<&HashMap<ServiceType, u64>>,
        service: &ServiceType,
        new_session_number: &u64,
    ) {
        // If `previous_sessions` is provided, check if the session number has changed.
        // Otherwise, always broadcast (for initialization).
        if previous_sessions.is_some_and(|prev| {
            prev.get(service)
                .copied()
                .expect("previous session number is set")
                == *new_session_number
        }) {
            return;
        }

        match cryptarchia.active_session_providers(block_id, *service) {
            Ok(providers) => {
                let update = SessionUpdate {
                    session_number: *new_session_number,
                    providers,
                };

                let broadcast_relay = relays.broadcast_relay();

                let broadcast_future = match service {
                    ServiceType::BlendNetwork => {
                        broadcast_blend_session(broadcast_relay, update).boxed()
                    }
                    ServiceType::DataAvailability => {
                        broadcast_da_session(broadcast_relay, update).boxed()
                    }
                };

                if let Err(e) = broadcast_future.await {
                    error!("Failed to broadcast session update for {service:?}: {e}");
                }
            }
            Err(e) => {
                error!("Could not get session providers for service {service:?}: {e}");
            }
        }
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

async fn broadcast_blend_session(
    broadcast_relay: &BroadcastRelay,
    session: SessionUpdate,
) -> Result<(), DynError> {
    broadcast_relay
        .send(BlockBroadcastMsg::BroadcastBlendSession(session))
        .await
        .map_err(|(error, _)| Box::new(error) as DynError)
}

async fn broadcast_da_session(
    broadcast_relay: &BroadcastRelay,
    session: SessionUpdate,
) -> Result<(), DynError> {
    broadcast_relay
        .send(BlockBroadcastMsg::BroadcastDASession(session))
        .await
        .map_err(|(error, _)| Box::new(error) as DynError)
}
