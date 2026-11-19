pub mod api;
mod bootstrap;
mod ingestion;
mod mempool;
mod metrics;
pub mod network;
mod relays;
mod sync;

use core::{fmt::Debug, num::NonZeroUsize, pin::pin};
use std::{collections::HashMap, fmt::Display, hash::Hash};

use bootstrap::ibd::ChainNetworkIbdBlockProcessor;
use futures::{FutureExt as _, StreamExt as _};
use lb_chain_service::{
    AppliedBlock, BlockWithVerifiedHeader, VerifiedBlockHeader,
    api::{ApiError, CryptarchiaServiceApi, CryptarchiaServiceData},
};
use lb_core::{
    block::{Block, BlockLimits, BlockTransactions, Proposal},
    header::HeaderId,
    mantle::{
        ledger::verification_mode::StandardMode,
        traits::{Hashable, SignedMantleTx, StorageSize},
        transactions::{
            hash::{TxHash, TxHashPrefix},
            states::Preverified,
        },
    },
};
use lb_cryptarchia_engine::EraSchedule;
pub use lb_cryptarchia_engine::{Epoch, Slot};
pub use lb_ledger::EpochState;
use lb_log_targets::chain;
use lb_network_service::{NetworkService, message::ChainSyncEvent};
use lb_services_utils::wait_until_services_are_ready;
use lb_storage_service::StorageService;
use lb_time_service::{TimeService, api::TimeServiceApi};
use lb_tx_service::{
    TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};
use lb_utils::{bounded::BoundedError, math::ProperFraction, tokio::task::spawn};
use network::NetworkAdapter;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{
    sync::{broadcast, oneshot},
    task::JoinHandle,
};
use tracing::{Level, debug, error, info, instrument, span, trace};
use tracing_futures::Instrument as _;

pub use crate::{
    bootstrap::config::{BootstrapConfig, IbdConfig},
    sync::config::{OrphanConfig, SyncConfig, TipPollConfig},
};
use crate::{
    bootstrap::ibd::InitialBlockDownload,
    ingestion::{BlockIngestion, IngestionHandle},
    mempool::{MempoolAdapter as MempoolAdapterTrait, adapter::MempoolAdapter},
    relays::ChainNetworkRelays,
    sync::{
        orphan_handler::OrphanBlocksDownloader,
        tip_poll::{TipPollParams, poll_peer_tips_if_behind},
    },
};

const SERVICE_ID: &str = "ChainNetwork";

pub(crate) const LOG_TARGET: &str = chain::network::ROOT;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Cryptarchia(#[from] ApiError),
    #[error("Serialization error: {0}")]
    Serialisation(#[from] lb_binary_codec::bincode::Error),
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("No combination of candidate transactions reproduces the block root")]
    NoMatchingReconstruction,
    #[error("Reference {index} ({prefix}) matches no local transaction")]
    UnresolvedReference { index: usize, prefix: TxHashPrefix },
    #[error("Reference {index} ({prefix}) matches more than one local transaction")]
    CollidingReference { index: usize, prefix: TxHashPrefix },
    #[error("Mempool error: {0}")]
    Mempool(String),
    #[error("Block header id not found: {0}")]
    HeaderIdNotFound(HeaderId),
    #[error(transparent)]
    BoundedError(#[from] BoundedError),
}

impl From<lb_core::block::Error> for Error {
    fn from(e: lb_core::block::Error) -> Self {
        match e {
            lb_core::block::Error::Serialisation(_)
            | lb_core::block::Error::Header(_)
            | lb_core::block::Error::BodyRootMismatch
            | lb_core::block::Error::BoundedError(_)
            | lb_core::block::Error::ContentTooBig { .. } => Self::InvalidBlock(e.to_string()),
        }
    }
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "Boxing a block in `ApplyBlockAndReconcileMempool` would cost in every apply an allocation to spare the rare subscription eight bytes."
)]
pub enum Message<Tx> {
    ApplyBlockAndReconcileMempool {
        block: Block<Tx>,
        resp: oneshot::Sender<Result<(), Error>>,
    },
    SubscribeToProposals {
        result_sender: oneshot::Sender<broadcast::Receiver<Proposal>>,
    },
}

#[derive(Debug, Clone)]
pub struct ChainNetworkSettings<NodeId, NetworkAdapterSettings>
where
    NodeId: Clone + Eq + Hash,
{
    pub network: NetworkAdapterSettings,
    pub bootstrap: BootstrapConfig<NodeId>,
    pub sync: SyncConfig,
    pub seen_items_subscription_channel_size: NonZeroUsize,
    /// What the chain network runs with in each era.
    pub era_schedule: EraSchedule<ChainNetworkEraSettings>,
}

/// What the chain network runs with in an era.
#[derive(Debug, Clone)]
pub struct ChainNetworkEraSettings {
    /// The era's active slot coefficient `f`: blocks are expected `1/f` slots
    /// apart, which sets how often the tip poll runs and how far the local tip
    /// may lag before it polls.
    pub slot_activation_coeff: ProperFraction,
    /// The most uncles, transactions and transaction bytes a block of the era
    /// may hold: what a block rebuilt from a proposal of the era is held to.
    pub block_limits: BlockLimits,
}

#[expect(clippy::allow_attributes_without_reason)]
pub struct ChainNetwork<
    Cryptarchia,
    NetAdapter,
    Mempool,
    MempoolNetAdapter,
    TimeBackend,
    RuntimeServiceId,
> where
    Cryptarchia: CryptarchiaServiceData<Tx = Mempool::Item>,
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::Backend: 'static,
    NetAdapter::Settings: Send,
    NetAdapter::PeerId: Clone + Eq + Hash,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Item: Clone + Eq + Debug + 'static,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode>,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolNetAdapter::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
    received_proposals_sender: broadcast::Sender<Proposal>,
}

impl<Cryptarchia, NetAdapter, Mempool, MempoolNetAdapter, TimeBackend, RuntimeServiceId> ServiceData
    for ChainNetwork<
        Cryptarchia,
        NetAdapter,
        Mempool,
        MempoolNetAdapter,
        TimeBackend,
        RuntimeServiceId,
    >
where
    Cryptarchia: CryptarchiaServiceData<Tx = Mempool::Item>,
    NetAdapter: NetworkAdapter<RuntimeServiceId>,
    NetAdapter::Settings: Send,
    NetAdapter::PeerId: Clone + Eq + Hash,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash>,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Item: SignedMantleTx<Preverified, StandardMode> + Clone + Eq + Debug,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolNetAdapter::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
{
    type Settings = ChainNetworkSettings<NetAdapter::PeerId, NetAdapter::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = Message<Mempool::Item>;
}

#[async_trait::async_trait]
impl<Cryptarchia, NetAdapter, Mempool, MempoolNetAdapter, TimeBackend, RuntimeServiceId>
    ServiceCore<RuntimeServiceId>
    for ChainNetwork<
        Cryptarchia,
        NetAdapter,
        Mempool,
        MempoolNetAdapter,
        TimeBackend,
        RuntimeServiceId,
    >
where
    Cryptarchia: CryptarchiaServiceData<Tx = Mempool::Item>,
    NetAdapter: NetworkAdapter<RuntimeServiceId, Block = Block<Mempool::Item>, Proposal = Proposal>
        + Clone
        + Send
        + Sync
        + 'static,
    NetAdapter::Settings: Send + Sync + 'static,
    NetAdapter::PeerId: Clone + Eq + Hash + Copy + Debug + Send + Sync + Unpin + 'static,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync + 'static,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Item: Hashable<Hash = Mempool::Key>
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + Unpin
        + 'static,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<Cryptarchia>
        + AsServiceId<NetworkService<NetAdapter::Backend, TimeBackend, RuntimeServiceId>>
        + AsServiceId<StorageService<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetAdapter,
                Mempool,
                Mempool::Storage,
                TimeBackend,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        let received_proposals_buffer = service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings()
            .seen_items_subscription_channel_size;
        let (received_proposals_sender, _) = broadcast::channel(received_proposals_buffer.get());
        Ok(Self {
            service_resources_handle,
            received_proposals_sender,
        })
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), DynError> {
        let mut relays: ChainNetworkRelays<
            Cryptarchia,
            Mempool,
            MempoolNetAdapter,
            NetAdapter,
            RuntimeServiceId,
        > = ChainNetworkRelays::from_service_resources_handle::<TimeBackend>(
            &self.service_resources_handle,
        )
        .await;

        let ChainNetworkSettings {
            network: network_config,
            bootstrap: bootstrap_config,
            sync: sync_config,
            era_schedule,
            ..
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            None,
            NetworkService<_, _, _>,
            TxMempoolService<_, _, _, _, _>,
            TimeService<_, _>,
            Cryptarchia
        )
        .await?;

        notify_mempool_of_highest_block::<_, Mempool>(
            relays.cryptarchia(),
            relays.mempool_adapter(),
        )
        .await;

        // The adapters start in the era of the slot the clock is in, follow
        // the eras its slot ticks enter after it, and drop what they keep of
        // the era before when its transition period ends.
        let time_service_api = TimeServiceApi::<TimeService<TimeBackend, RuntimeServiceId>>::new(
            &self.service_resources_handle.overwatch_handle,
        )
        .await?;
        let (current_slot_tick, mut slot_ticks) =
            time_service_api.subscribe_to_epoch_slots().await?;
        let mut era_transition_ends = time_service_api.subscribe_to_era_transition_ends().await?;
        let mut current_era = current_slot_tick.era;

        let mut network_adapter =
            NetAdapter::new(network_config, current_era, relays.network_relay().clone()).await;

        let initial_block_download = InitialBlockDownload::new(
            ChainNetworkIbdBlockProcessor::<_, Mempool> {
                cryptarchia: relays.cryptarchia().clone(),
                mempool_adapter: relays.mempool_adapter().clone(),
            },
            network_adapter.clone(),
        );

        // The adapters keep following the clock while the initial block
        // download runs, so they never fall behind its eras. The download
        // works on its own copies of them, through chain sync only.
        let mut initial_block_download_task =
            pin!(initial_block_download.run(bootstrap_config.ibd, &sync_config.orphan));
        let initial_block_download_result = loop {
            tokio::select! {
                result = &mut initial_block_download_task => break result,
                Some(tick) = slot_ticks.next() => {
                    if tick.era > current_era {
                        current_era = tick.era;
                        network_adapter.rotate_era(current_era).await;
                        relays.mempool_adapter_mut().rotate_era(current_era).await;
                    }
                }
                Some(transition_end) = era_transition_ends.next() => {
                    network_adapter.complete_era_transition(transition_end.era()).await;
                    relays.mempool_adapter_mut().complete_era_transition(transition_end.era()).await;
                }
            }
        };

        match initial_block_download_result {
            Ok(_) => {
                info!(target: LOG_TARGET, "Initial Block Download completed successfully");
                // Notify chain-service that IBD is complete so it can start the
                // prolonged bootstrap timer
                if let Err(e) = relays.cryptarchia().notify_ibd_completed().await {
                    error!(target: LOG_TARGET, "Failed to notify chain-service of IBD completion: {e:?}");
                }
            }
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    "Initial Block Download failed: {e:?}. Initiating graceful shutdown. Retry with different bootstrap peers"
                );
                if let Err(shutdown_err) = self
                    .service_resources_handle
                    .overwatch_handle
                    .shutdown()
                    .await
                {
                    error!(target: LOG_TARGET, "Failed to shutdown overwatch: {shutdown_err:?}");
                }

                return Err(DynError::from(format!(
                    "Initial Block Download failed: {e:?}"
                )));
            }
        }

        let mut incoming_proposals = network_adapter.proposals_stream().await?;
        let mut chainsync_forwarding = pin!(
            forward_chainsync_events(
                network_adapter.chainsync_events_stream().await?,
                relays.cryptarchia(),
            )
            .fuse()
        );

        // Blocks go through ingestion from here on, a future this loop polls
        // along with its other inputs, so that the loop only routes and is
        // never held up by a block.
        let (ingestion, ingestion_run) = BlockIngestion::new(
            relays.cryptarchia().clone(),
            relays.mempool_adapter().clone(),
            OrphanBlocksDownloader::new(
                network_adapter.clone(),
                sync_config.orphan.max_orphan_cache_size,
                sync_config.orphan.max_rejected_cache_size,
            ),
            era_schedule.map(|era| era.parameters.block_limits),
        )
        .start();
        let mut ingestion_run = pin!(ingestion_run);

        // Set up the proactive tip-poll lag watchdog: in each era, derive the
        // polling cadence / lag threshold from the era's active slot
        // coefficient `f`. When polling is disabled, the watchdog stays inert
        // and slot ticks only drive the eras.
        let tip_poll_params = sync_config.tip_poll.enabled.then(|| {
            era_schedule.map(|era| {
                let params = TipPollParams::derive(
                    &sync_config.tip_poll,
                    era.parameters.slot_activation_coeff,
                );
                info!(
                    target: LOG_TARGET,
                    era = era.era.into_inner(),
                    cadence_slots = params.cadence_slots,
                    lag_threshold_slots = params.lag_threshold_slots,
                    max_peers = params.max_peers,
                    "Tip-poll lag watchdog enabled"
                );
                params
            })
        });

        let mut tip_poll_task: Option<JoinHandle<()>> = None;

        self.notify_service_ready();

        let async_loop = async {
            loop {
                tokio::select! {
                    Some(proposal) = incoming_proposals.next() => {
                        self.note_received_proposal(&proposal);
                        ingestion.enqueue_proposal(proposal);
                    }

                    Some(tick) = slot_ticks.next() => {
                        if tick.era > current_era {
                            current_era = tick.era;
                            network_adapter.rotate_era(current_era).await;
                            ingestion.rotate_era(current_era);
                        }

                        let Some(params) = tip_poll_params
                            .as_ref()
                            .map(|params| *params.at_slot(tick.slot).parameters)
                        else {
                            continue;
                        };
                        // Don't start a new poll if the previous one is still running.
                        if tip_poll_task.as_ref().is_some_and(|handle| !handle.is_finished()) {
                            continue;
                        }

                        // Spawn a task to not block this event loop.
                        // The task makes tip requests handled by this event loop in peers' side.
                        // All two nodes make tip requests simultaneously to each other,
                        // `poll_peer_tips_if_behind` will cause deadlock unless it's spawned off.
                        let adapter = network_adapter.clone();
                        let cryptarchia = relays.cryptarchia().clone();
                        let tx = ingestion.polled_tips();
                        tip_poll_task = Some(spawn("logos/chain/tip-poll", async move {
                            if let Some(polled) = poll_peer_tips_if_behind(
                                &adapter,
                                &cryptarchia,
                                tick,
                                &params,
                            )
                            .await && let Err(e) = tx.send(polled).await {
                                error!(target: LOG_TARGET, %e, "the polled-tip receiver has been dropped");
                            }
                        }));
                    }

                    Some(transition_end) = era_transition_ends.next() => {
                        network_adapter.complete_era_transition(transition_end.era()).await;
                        ingestion.complete_era_transition(transition_end.era());
                    }

                    // Any message can be a local block, which has to find room
                    // in ingestion's queue.
                    Some(msg) = self.service_resources_handle.inbound_relay.next(), if ingestion.has_room_for_local_block() => {
                        Self::handle_message(msg, &self.received_proposals_sender, &ingestion);
                    }

                    () = &mut chainsync_forwarding => {}

                    never = &mut ingestion_run => match never {},
                }
            }
        };

        // It sucks to use `SERVICE_ID` when we have `<RuntimeServiceId as
        // AsServiceId<Self>>::SERVICE_ID`.
        // Somehow it just does not let us use it.
        //
        // Hypothesis:
        // 1. Probably related to too many generics.
        // 2. It seems `span` requires a `const` string literal.
        async_loop
            .instrument(span!(target: LOG_TARGET, Level::TRACE, SERVICE_ID))
            .await;

        Ok(())
    }
}

impl<Cryptarchia, NetAdapter, Mempool, MempoolNetAdapter, TimeBackend, RuntimeServiceId>
    ChainNetwork<Cryptarchia, NetAdapter, Mempool, MempoolNetAdapter, TimeBackend, RuntimeServiceId>
where
    Cryptarchia: CryptarchiaServiceData<Tx = Mempool::Item>,
    NetAdapter: NetworkAdapter<RuntimeServiceId, Block = Block<Mempool::Item>, Proposal = Proposal>
        + Clone
        + Send
        + Sync
        + 'static,
    NetAdapter::Settings: Send + Sync + 'static,
    NetAdapter::PeerId: Clone + Eq + Hash + Copy + Debug + Send + Sync,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync + 'static,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Item: Hashable<Hash = Mempool::Key>
        + SignedMantleTx<Preverified, StandardMode>
        + StorageSize
        + Debug
        + Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolNetAdapter::Settings: Send + Sync,
    TimeBackend: lb_time_service::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
    RuntimeServiceId: Display + AsServiceId<Self> + Sync,
{
    fn notify_service_ready(&self) {
        self.service_resources_handle.status_updater.notify_ready();
        info!(
            target: LOG_TARGET,
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );
    }

    fn note_received_proposal(&self, proposal: &Proposal) {
        if self.received_proposals_sender.receiver_count() > 0 {
            drop(self.received_proposals_sender.send(proposal.clone()));
        }
    }

    fn handle_message(
        msg: Message<Mempool::Item>,
        received_proposals: &broadcast::Sender<Proposal>,
        ingestion: &IngestionHandle<Mempool::Item>,
    ) {
        match msg {
            Message::SubscribeToProposals { result_sender } => {
                if result_sender.send(received_proposals.subscribe()).is_err() {
                    error!(
                        target: LOG_TARGET,
                        "Subscriber hung up before it could be given the received-proposal stream."
                    );
                }
            }
            Message::ApplyBlockAndReconcileMempool { block, resp } => {
                ingestion.apply_local_block(block, resp);
            }
        }
    }
}

/// Hands chain sync requests to the chain service, one after the other. The
/// service loop polls this along with its other inputs, so that a busy chain
/// service holds up nothing else.
async fn forward_chainsync_events<Cryptarchia>(
    events: network::BoxedStream<ChainSyncEvent>,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
) where
    Cryptarchia: CryptarchiaServiceData<Tx: Send>,
{
    events
        .for_each(async |event| {
            if let Err(e) = cryptarchia.handle_chainsync_event(event).await {
                error!(target: LOG_TARGET, "Failed to forward chainsync event to chain-service: {e}");
            }
        })
        .await;
}

async fn should_process_block<Cryptarchia>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    block_id: HeaderId,
    block_slot: Slot,
) -> Result<(), DoNotProcessBlock>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: SignedMantleTx<Preverified, StandardMode> + Debug + Clone + Send,
{
    if !is_after_lib(cryptarchia, block_id, block_slot).await {
        return Err(DoNotProcessBlock::OlderThanLib);
    }

    match cryptarchia.get_ledger_state(block_id).await {
        Ok(Some(_)) => Err(DoNotProcessBlock::AlreadyApplied),
        Ok(None) => Ok(()),
        Err(err) => {
            error!(target: LOG_TARGET, err = ?err, "Failure when checking if block already processed");
            // block processing is idempotent, so we can safely re-process a
            // block
            Ok(())
        }
    }
}

/// Errors that indicate why a block should not be processed.
#[derive(Debug)]
enum DoNotProcessBlock {
    OlderThanLib,
    AlreadyApplied,
}

async fn is_after_lib<Cryptarchia>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    block_id: HeaderId,
    block_slot: Slot,
) -> bool
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: SignedMantleTx<Preverified, StandardMode> + Debug + Clone + Send,
{
    match cryptarchia.info().await {
        Ok(info) => {
            if !is_at_or_before_lib(block_slot, info.cryptarchia_info.lib_slot) {
                return true;
            }

            trace!(
                target: LOG_TARGET,
                ?block_id,
                ?block_slot,
                lib = ?info.cryptarchia_info.lib,
                lib_slot = ?info.cryptarchia_info.lib_slot,
                "Ignoring block at or before local LIB"
            );
            false
        }
        Err(err) => {
            error!(target: LOG_TARGET, err = ?err, "Failure when checking local LIB");
            true
        }
    }
}

fn is_at_or_before_lib(block_slot: Slot, lib_slot: Slot) -> bool {
    block_slot <= lib_slot
}

/// Apply-time errors that must NOT mark the block as rejected:
/// - `ParentMissing` / `FutureBlock`: transient — the block may become
///   applicable once an ancestor arrives or the local clock catches up.
/// - `AlreadyApplied`: the block is already in the chain; re-processing is
///   unnecessary. Rejecting would also block its valid descendants from
///   entering the orphan pipeline.
/// - `CommsFailure`: relay failure has nothing to do with block validity; a
///   blanket "reject" here would poison the cache whenever chain-service is
///   temporarily unreachable.
/// - `Storage`: a local storage failure says nothing of the block either, which
///   is applied again once the storage recovers.
/// - `InvalidLeaderProof` / `InvalidOperationProof`: a bad copy, discarded
///   without a verdict against the block ID, which does not cover these proofs.
///   The genuine block shares the ID.
/// - `InvalidUncles` / `InvalidBody`: they condemn only the received copy. The
///   uncles are verified with the header, before the body root confirms that
///   they are the ones the block ID commits to. A genuine copy may arrive
///   later.
///
/// A `reward_value` differing from the computed reward is terminal: the block
/// ID covers both the value and the body that `BlockWithVerifiedHeader::new`
/// confirmed.
///
/// Other apply errors (e.g. validation failures surfaced as
/// `ApiError::Unexpected`) are treated as terminal for the orphan pipeline.
const fn is_recoverable_apply_error(err: &Error) -> bool {
    matches!(
        err,
        Error::Cryptarchia(
            ApiError::ParentMissing { .. }
                | ApiError::FutureBlock { .. }
                | ApiError::AlreadyApplied(_)
                | ApiError::CommsFailure(_)
                | ApiError::InvalidLeaderProof
                | ApiError::InvalidOperationProof(_)
                | ApiError::InvalidUncles(_)
                | ApiError::InvalidBody(_)
                | ApiError::Storage { .. },
        ),
    )
}

/// Pass the highest slot of the block tree to the mempool service.
async fn notify_mempool_of_highest_block<Cryptarchia, Mempool>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    mempool: &MempoolAdapter<Mempool::Item>,
) where
    Cryptarchia: CryptarchiaServiceData<Tx: Send>,
    Mempool:
        RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = Cryptarchia::Tx> + Send + Sync,
{
    // The genesis slot as the LIB releases nothing.
    match cryptarchia.highest_block().await {
        Ok((highest_id, highest_slot)) => {
            if let Err(err) = mempool
                .notify_processed_block(highest_id, highest_slot, Slot::genesis())
                .await
            {
                error!(target: LOG_TARGET, err, "could not give the mempool the highest block");
            }
        }
        Err(err) => {
            error!(target: LOG_TARGET, ?err, "could not read the highest block for the mempool");
        }
    }
}

/// Try to add a [`Block`] to [`Cryptarchia`], verifying its header first.
/// A [`Block`] is only added if it's valid
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip(block, cryptarchia, mempool),
    fields(block_id = %block.header().id(), tx_count = block.transactions().len())
)]
async fn apply_block_and_reconcile_mempool<Cryptarchia>(
    block: Block<Cryptarchia::Tx>,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    mempool: &MempoolAdapter<Cryptarchia::Tx>,
) -> Result<(), Error>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx:
        SignedMantleTx<Preverified, StandardMode> + StorageSize + Debug + Clone + Send + 'static,
{
    let (applied, block_id, block_slot) = apply_block_to_chain(block, cryptarchia).await?;
    reconcile_mempool(applied, block_id, block_slot, mempool).await;
    Ok(())
}

/// Try to add a [`Block`] to [`Cryptarchia`], verifying its header first.
/// What it changed comes back with the block's ID and slot, for the mempool
/// to follow with [`reconcile_mempool`].
async fn apply_block_to_chain<Cryptarchia>(
    block: Block<Cryptarchia::Tx>,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
) -> Result<(AppliedBlock<Cryptarchia::Tx>, HeaderId, Slot), Error>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx:
        SignedMantleTx<Preverified, StandardMode> + StorageSize + Debug + Clone + Send + 'static,
{
    trace!(target: LOG_TARGET, "Received proposal with ID: {:?}", block.header().id());

    let header = cryptarchia
        .verify_header(block.header().clone(), block.uncle_headers().clone())
        .await?;
    // The count of the transactions was held to the block's era when the block
    // was decoded, and the ledger holds it to that era again when applying it.
    let transactions = BlockTransactions::try_new(block.into_transactions(), u16::MAX)?;
    let block = BlockWithVerifiedHeader::new(header, transactions)
        .map_err(|e| Error::Cryptarchia(ApiError::InvalidBody(e.to_string())))?;
    let (block_id, block_slot) = (block.header().id(), block.header().slot());
    let applied = cryptarchia.apply_block(block).await?;
    Ok((applied, block_id, block_slot))
}

/// Try to add a [`BlockWithVerifiedHeader`] to [`Cryptarchia`], as
/// [`apply_block_and_reconcile_mempool`] does for a [`Block`].
#[instrument(
    target = LOG_TARGET,
    level = "debug",
    skip(block, cryptarchia, mempool),
    fields(block_id = %block.header().id(), tx_count = block.transactions_iter().len())
)]
async fn apply_block_with_verified_header_and_reconcile_mempool<Cryptarchia>(
    block: BlockWithVerifiedHeader<Cryptarchia::Tx>,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia>,
    mempool: &MempoolAdapter<Cryptarchia::Tx>,
) -> Result<(), Error>
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: SignedMantleTx<Preverified, StandardMode> + Debug + Clone + Send + 'static,
{
    let block_id = block.header().id();
    let block_slot = block.header().slot();
    let applied = cryptarchia.apply_block(block).await?;
    reconcile_mempool(applied, block_id, block_slot, mempool).await;
    Ok(())
}

/// Lets the mempool follow how applying a block changed the canonical chain.
#[expect(
    clippy::cognitive_complexity,
    reason = "the logging macros of the two mempool notifications count as branches"
)]
async fn reconcile_mempool<Tx>(
    AppliedBlock {
        tip,
        newly_canonical_txs,
        reorged_txs,
        lib_slot,
    }: AppliedBlock<Tx>,
    block_id: HeaderId,
    block_slot: Slot,
    mempool: &MempoolAdapter<Tx>,
) where
    Tx: SignedMantleTx<Preverified, StandardMode> + Debug + Clone + Send + 'static,
{
    debug!(
        target: LOG_TARGET, ?block_id, ?tip,
        retired_txs = newly_canonical_txs.len(), reinserted_txs = reorged_txs.len(),
        "block applied; reconciling mempool"
    );

    // Retire txs of every block that entered the canonical chain, and let the
    // mempool admit the reorged txs again.
    if !(newly_canonical_txs.is_empty() && reorged_txs.is_empty())
        && let Err(err) = mempool
            .notify_canonical_chain_updated(newly_canonical_txs, reorged_txs)
            .await
    {
        error!(target: LOG_TARGET, err, "could not notify mempool of canonical chain update");
    }

    // Notify the mempool to trigger evictions and release txs whose block
    // became immutable.
    if let Err(err) = mempool
        .notify_processed_block(block_id, block_slot, lib_slot)
        .await
    {
        error!(target: LOG_TARGET, err, "could not notify mempool of processed block");
    }
}

/// Reconstruct a block from the verified header of a proposal by resolving
/// the proposal's reference prefixes against the local mempool.
///
/// A reference is a 16-byte prefix of the transaction hash. Two distinct hashes
/// cannot be found or manufactured to share one at any feasible cost, so every
/// reference resolves to exactly one local transaction and resolution never
/// branches. `header.body_root` still commits to the full hashes and
/// [`BlockWithVerifiedHeader::new`] checks it, which is what catches the
/// residual, cryptographically negligible case of a prefix matching the wrong
/// transaction.
///
/// Taking a [`VerifiedBlockHeader`] means the header was validated first: this
/// reads the mempool for every reference, so an unauthenticated proposal has
/// to be discarded before it gets here. The block is held to the transaction
/// count of `block_limits`, those of the era of the proposal's slot: the
/// proposal's uncle headers were decoded under them, which the rebuilt block
/// keeps. The ledger holds the transactions, and the data of their Operations,
/// to the block's era when it applies the block.
async fn reconstruct_block_from_proposal<Item>(
    header: VerifiedBlockHeader,
    references: &[TxHashPrefix],
    block_limits: BlockLimits,
    mempool: &impl MempoolAdapterTrait<Item>,
) -> Result<BlockWithVerifiedHeader<Item>, Error>
where
    Item: SignedMantleTx<Preverified, StandardMode> + StorageSize + Clone + Send + Sync + 'static,
{
    let transactions = resolve_references(references, mempool).await?;

    BlockWithVerifiedHeader::new(
        header,
        BlockTransactions::try_new(transactions, block_limits.max_transactions)
            .map_err(|_| Error::NoMatchingReconstruction)?,
    )
    .map_err(|_| Error::NoMatchingReconstruction)
}

/// The single local transaction each reference means, in the order of the
/// references. The first one that doesn't resolve is the one reported.
///
/// A reference resolves only when the match is unique. Zero matches means the
/// transaction has not reached this mempool; two or more would be a prefix
/// collision, which is infeasible to manufacture and vanishingly unlikely to
/// occur by chance, and is treated as unresolved rather than searched. Because
/// the match is unique when it exists, the result does not depend on the order
/// in which the mempool is scanned.
///
/// The references go to the mempool together, in one round trip, and what it
/// finds comes back in no particular order.
async fn resolve_references<Item>(
    references: &[TxHashPrefix],
    mempool: &impl MempoolAdapterTrait<Item>,
) -> Result<Vec<Item>, Error>
where
    Item: Hashable<Hash = TxHash> + Clone + Send + Sync + 'static,
{
    if references.is_empty() {
        return Ok(Vec::new());
    }

    let mut matches = mempool
        .get_transactions_by_prefixes(references.iter().copied())
        .await
        .map_err(|e| Error::Mempool(format!("Failed to resolve references: {e}")))?;
    let mut candidates: HashMap<TxHashPrefix, Vec<Item>> = HashMap::new();
    while let Some(item) = matches.next().await {
        candidates
            .entry(item.hash().prefix())
            .or_default()
            .push(item);
    }

    references
        .iter()
        .copied()
        .enumerate()
        .map(
            |(index, prefix)| match candidates.get(&prefix).map(Vec::as_slice) {
                // Only one match
                Some([item]) => Ok(item.clone()),
                // At least two matches
                Some([_, _, ..]) => Err(Error::CollidingReference { index, prefix }),
                // No match
                None | Some([]) => {
                    metrics::consensus_observe_proposal_missing_txs(1);
                    Err(Error::UnresolvedReference { index, prefix })
                }
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use futures::stream;
    use lb_binary_codec::canonical::BinaryDecodeExt as _;
    use lb_core::{
        block::{TxInBlock, UncleHeaders},
        mantle::{traits::Hasher, transactions::hash::REFERENCE_PREFIX_BYTES},
        proofs::leader_proof::Groth16LeaderProof,
    };
    use lb_cryptarchia_engine::Era;
    use lb_cryptarchia_sync::TipResponse;
    use lb_network_service::{backends::mock::Mock, message::BackendNetworkMsg};
    use lb_tx_service::TxsByPrefix;
    use overwatch::services::relay::OutboundRelay;

    use super::*;
    use crate::network::BoxedStream;

    /// A transaction that is nothing but its hash, which is all
    /// [`resolve_references`] looks at.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct HashOnlyTx(TxHash);

    impl Hashable for HashOnlyTx {
        const HASHER: Hasher<Self> = |tx| tx.0;
        type Hash = TxHash;

        fn as_signing(&self) -> Vec<u8> {
            self.0.0.to_vec()
        }
    }

    /// A mempool that answers every prefix with the same fixed candidate list,
    /// so a test states only how many transactions a reference matches.
    struct FixedCandidates(Vec<HashOnlyTx>);

    #[async_trait::async_trait]
    impl MempoolAdapterTrait<HashOnlyTx> for FixedCandidates {
        async fn rotate_era(&mut self, _era: Era) {}

        async fn complete_era_transition(&mut self, _ended_era: Era) {}

        async fn notify_canonical_chain_updated(
            &self,
            _entered: HashMap<TxHash, Slot>,
            _reorged: Vec<TxInBlock<HashOnlyTx>>,
        ) -> Result<(), DynError> {
            unimplemented!("resolution never changes the canonical chain")
        }

        async fn notify_processed_block(
            &self,
            _block_id: HeaderId,
            _slot: Slot,
            _lib_slot: Slot,
        ) -> Result<(), DynError> {
            unimplemented!("resolution never processes blocks")
        }

        async fn get_transactions_by_prefixes<Prefixes>(
            &self,
            prefixes: Prefixes,
        ) -> Result<TxsByPrefix<HashOnlyTx>, DynError>
        where
            Prefixes: IntoIterator<Item = TxHashPrefix> + Send,
        {
            assert!(
                prefixes.into_iter().next().is_some(),
                "there was nothing to look up"
            );
            Ok(Box::pin(stream::iter(self.0.clone())))
        }
    }

    fn tx(byte: u8) -> HashOnlyTx {
        HashOnlyTx(TxHash([byte; 32]))
    }

    /// Two transactions that agree on the reference prefix and differ only past
    /// it — the collision a 16-byte prefix makes infeasible, constructed by
    /// hand.
    fn colliding_pair() -> Vec<HashOnlyTx> {
        let mut first = [0xAAu8; 32];
        let mut second = [0xAAu8; 32];
        first[REFERENCE_PREFIX_BYTES] = 0x01;
        second[REFERENCE_PREFIX_BYTES] = 0x02;
        vec![HashOnlyTx(TxHash(first)), HashOnlyTx(TxHash(second))]
    }

    fn prefix_of(byte: u8) -> TxHashPrefix {
        TxHash([byte; 32]).prefix()
    }

    /// An empty block has no reason to wait on a busy mempool.
    #[tokio::test]
    async fn a_proposal_without_references_leaves_the_mempool_alone() {
        let resolved: Vec<HashOnlyTx> = resolve_references(&[], &FixedCandidates(Vec::new()))
            .await
            .expect("there is nothing to resolve");

        assert_eq!(resolved, vec![]);
    }

    #[tokio::test]
    async fn references_resolve_in_their_own_order() {
        let mempool = FixedCandidates(vec![tx(8), tx(7)]);

        let resolved = resolve_references(&[prefix_of(7), prefix_of(8)], &mempool)
            .await
            .expect("unique matches resolve");

        assert_eq!(resolved, [tx(7), tx(8)]);
    }

    /// A proposal that refers to one transaction twice is for
    /// `Block::reconstruct` to judge, not for resolution.
    #[tokio::test]
    async fn two_references_to_one_transaction_both_resolve() {
        let mempool = FixedCandidates(vec![tx(7)]);

        let resolved = resolve_references(&[prefix_of(7), prefix_of(7)], &mempool)
            .await
            .expect("a unique match resolves");

        assert_eq!(resolved, [tx(7), tx(7)]);
    }

    #[tokio::test]
    async fn a_reference_matching_nothing_locally_is_unresolved() {
        let mempool = FixedCandidates(vec![tx(1)]);
        let prefix = prefix_of(7);

        assert!(matches!(
            resolve_references(&[prefix_of(1), prefix], &mempool).await,
            Err(Error::UnresolvedReference { index: 1, prefix: p }) if p == prefix
        ));
    }

    /// A collision is reported rather than searched: resolution must not
    /// branch, so that two validators holding the same mempool always
    /// decide alike.
    #[tokio::test]
    async fn a_reference_matching_two_transactions_is_not_searched() {
        let candidates = colliding_pair();
        let prefix = candidates[0].hash().prefix();
        assert_eq!(
            prefix,
            candidates[1].hash().prefix(),
            "the pair must collide"
        );

        let mempool = FixedCandidates([candidates, vec![tx(1)]].concat());

        assert!(matches!(
            resolve_references(&[prefix_of(1), prefix], &mempool).await,
            Err(Error::CollidingReference { index: 1, prefix: p }) if p == prefix
        ));
    }

    /// Which reference gets reported can't depend on the mempool's order, as
    /// two validators holding the same mempool have to decide alike.
    #[tokio::test]
    async fn the_first_reference_that_does_not_resolve_is_the_one_reported() {
        let candidates = colliding_pair();
        let colliding = candidates[0].hash().prefix();
        let missing = prefix_of(7);
        let mempool = FixedCandidates(candidates);

        assert!(matches!(
            resolve_references(&[missing, colliding], &mempool).await,
            Err(Error::UnresolvedReference { index: 0, prefix: p }) if p == missing
        ));
    }

    /// Failures in what the block ID does not commit to (the proofs, and the
    /// uncles and the transactions before the body root confirms them) condemn
    /// only the received copy, so they must not get the block ID rejected: a
    /// genuine copy may arrive later.
    #[test]
    fn proof_uncle_and_body_failures_do_not_condemn_the_block() {
        for err in [
            ApiError::InvalidLeaderProof,
            ApiError::InvalidOperationProof(HeaderId::from([1; 32])),
            ApiError::InvalidUncles(String::new()),
            ApiError::InvalidBody(String::new()),
        ] {
            assert!(is_recoverable_apply_error(&Error::Cryptarchia(err)));
        }
        for err in [
            ApiError::InvalidHeader(String::new()),
            ApiError::InvalidSlot {
                parent: Slot::new(1),
                block: Slot::new(1),
            },
            ApiError::Unexpected(String::new()),
        ] {
            assert!(!is_recoverable_apply_error(&Error::Cryptarchia(err)));
        }
    }

    /// The block ID covers the reward value and, through the body root every
    /// applied block has confirmed, the transactions the reward is computed
    /// from. The chain service reports this failure as `Unexpected`, which the
    /// callers of `is_recoverable_apply_error` cache as rejected.
    #[test]
    fn wrong_reward_value_reported_as_unexpected_is_not_recoverable() {
        assert!(!is_recoverable_apply_error(&Error::Cryptarchia(
            ApiError::Unexpected(
                lb_ledger::LedgerError::<HeaderId>::WrongRewardValue {
                    header: 1,
                    computed: 2,
                }
                .to_string()
            )
        )));
    }

    /// A local storage failure is no verdict on the block, which is applied
    /// again once the storage recovers.
    #[test]
    fn local_storage_failure_is_not_cached_by_orphan_downloader() {
        assert!(is_recoverable_apply_error(&Error::Cryptarchia(
            ApiError::Storage {
                id: HeaderId::from([1; 32]),
                reason: "the disk is full".to_owned(),
            }
        )));
    }

    /// A dummy block at `slot`.
    pub fn block<Tx>(slot: u64) -> Block<Tx>
    where
        Tx: Hashable<Hash = TxHash> + StorageSize,
    {
        Block::create(
            HeaderId::from([0; 32]),
            Slot::new(slot),
            UncleHeaders::empty(),
            leader_proof(),
            BlockTransactions::empty(),
            u32::MAX,
        )
        .unwrap()
    }

    /// A network adapter that does nothing but serve the orphan downloader
    /// `download`, whatever it asks for.
    #[derive(Clone)]
    pub struct NoopNetworkAdapter<Block> {
        download: Vec<(HeaderId, Block)>,
    }

    impl<Block> NoopNetworkAdapter<Block> {
        pub const fn new(download: Vec<(HeaderId, Block)>) -> Self {
            Self { download }
        }
    }

    #[async_trait::async_trait]
    impl<RuntimeServiceId, Block> NetworkAdapter<RuntimeServiceId> for NoopNetworkAdapter<Block>
    where
        RuntimeServiceId: Send + Sync,
        Block: Clone + Send + Sync + 'static,
    {
        type Backend = Mock;
        type Settings = ();
        type PeerId = ();
        type Block = Block;
        type Proposal = ();

        async fn new(
            _settings: Self::Settings,
            _current_era: Era,
            _network_relay: OutboundRelay<BackendNetworkMsg<Self::Backend, RuntimeServiceId>>,
        ) -> Self {
            unimplemented!()
        }

        async fn rotate_era(&mut self, _era: Era) {}

        async fn complete_era_transition(&mut self, _ended_era: Era) {}

        async fn proposals_stream(&self) -> Result<BoxedStream<Self::Proposal>, DynError> {
            unimplemented!()
        }

        async fn chainsync_events_stream(&self) -> Result<BoxedStream<ChainSyncEvent>, DynError> {
            unimplemented!()
        }

        async fn request_tip(&self, _peer: Self::PeerId) -> Result<TipResponse, DynError> {
            unimplemented!()
        }

        async fn sample_tips(&self, _max_peers: usize) -> BoxedStream<TipResponse> {
            unimplemented!()
        }

        async fn request_blocks_from_peer(
            &self,
            _peer: Self::PeerId,
            _target_block: HeaderId,
            _local_tip: HeaderId,
            _latest_immutable_block: HeaderId,
            _additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            unimplemented!()
        }

        async fn request_blocks_from_peers(
            &self,
            _target_block: HeaderId,
            _local_tip: HeaderId,
            _latest_immutable_block: HeaderId,
            _additional_blocks: HashSet<HeaderId>,
        ) -> Result<BoxedStream<Result<(HeaderId, Self::Block), DynError>>, DynError> {
            Ok(Box::new(stream::iter(
                self.download.clone().into_iter().map(Ok),
            )))
        }
    }

    /// A dummy leader proof, which nothing here verifies.
    fn leader_proof() -> Groth16LeaderProof {
        Groth16LeaderProof::decode_all(&[0; Groth16LeaderProof::CANONICAL_ENCODED_SIZE])
            .expect("leader proof bytes must decode")
    }
}
