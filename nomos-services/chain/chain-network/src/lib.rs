mod blob;
mod bootstrap;
mod mempool;
pub mod network;
mod relays;
mod sync;

use core::fmt::Debug;
use std::{fmt::Display, hash::Hash, time::Duration};

use bootstrap::ibd::ChainNetworkIbdBlockProcessor;
use chain_service::api::{CryptarchiaServiceApi, CryptarchiaServiceData};
pub use cryptarchia_engine::{Epoch, Slot};
use futures::StreamExt as _;
use network::NetworkAdapter;
use nomos_core::{
    block::{Block, Proposal},
    da::{self},
    header::HeaderId,
    mantle::{AuthenticatedMantleTx, Transaction, TxHash, genesis_tx::GenesisTx, ops::Op},
    sdp::ServiceType,
};
use nomos_da_sampling::{
    DaSamplingService, DaSamplingServiceMsg, backend::DaSamplingServiceBackend,
    mempool::DaMempoolAdapter,
};
pub use nomos_ledger::EpochState;
use nomos_ledger::LedgerState;
use nomos_network::NetworkService;
use nomos_time::TimeService;
use overwatch::{
    DynError, OpaqueServiceResourcesHandle,
    services::{
        AsServiceId, ServiceCore, ServiceData,
        relay::OutboundRelay,
        state::{NoOperator, NoState},
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use services_utils::wait_until_services_are_ready;
use thiserror::Error;
use tracing::{Level, debug, error, info, instrument, span};
use tracing_futures::Instrument as _;
use tx_service::{
    TxMempoolService, backend::RecoverableMempool,
    network::NetworkAdapter as MempoolNetworkAdapter, storage::MempoolStorageAdapter,
};

use crate::{
    blob::{HistoricBlobStrategy, RecentBlobStrategy},
    bootstrap::ibd::InitialBlockDownload,
    mempool::{MempoolAdapter as _, adapter::MempoolAdapter},
    relays::ChainNetworkRelays,
    sync::orphan_handler::OrphanBlocksDownloader,
};
pub use crate::{
    bootstrap::config::{BootstrapConfig, IbdConfig},
    sync::config::{OrphanConfig, SyncConfig},
};

type SamplingRelay<BlobId> = OutboundRelay<DaSamplingServiceMsg<BlobId>>;

const CRYPTARCHIA_ID: &str = "Cryptarchia";

pub(crate) const LOG_TARGET: &str = "cryptarchia::service";

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Cryptarchia(#[from] chain_service::api::ApiError),
    #[error("Serialization error: {0}")]
    Serialisation(#[from] nomos_core::codec::Error),
    #[error("Invalid block: {0}")]
    InvalidBlock(String),
    #[error("Mempool error: {0}")]
    Mempool(String),
    #[error("Blob validation failed: {0}")]
    BlobValidationFailed(#[from] blob::Error),
    #[error("Block header id not found: {0}")]
    HeaderIdNotFound(HeaderId),
    #[error("Service session not found: {0:?}")]
    ServiceSessionNotFound(ServiceType),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChainNetworkSettings<NodeId, NetworkAdapterSettings>
where
    NodeId: Clone + Eq + Hash,
{
    pub config: nomos_ledger::Config,
    pub network_adapter_settings: NetworkAdapterSettings,
    pub bootstrap: BootstrapConfig<NodeId>,
    pub sync: SyncConfig,
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

#[expect(clippy::allow_attributes_without_reason)]
pub struct ChainNetwork<
    Cryptarchia,
    NetAdapter,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
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
    Mempool::Item: AuthenticatedMantleTx,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolDaAdapter: DaMempoolAdapter,
    MempoolNetAdapter::Settings: Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
{
    service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
}

impl<
    Cryptarchia,
    NetAdapter,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    RuntimeServiceId,
> ServiceData
    for ChainNetwork<
        Cryptarchia,
        NetAdapter,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
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
    Mempool::Item: AuthenticatedMantleTx + Clone + Eq + Debug,
    MempoolNetAdapter:
        MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>,
    MempoolDaAdapter: DaMempoolAdapter,
    MempoolNetAdapter::Settings: Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
{
    type Settings = ChainNetworkSettings<NetAdapter::PeerId, NetAdapter::Settings>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ();
}

#[async_trait::async_trait]
impl<
    Cryptarchia,
    NetAdapter,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    RuntimeServiceId,
> ServiceCore<RuntimeServiceId>
    for ChainNetwork<
        Cryptarchia,
        NetAdapter,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
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
    Mempool::Item: Transaction<Hash = Mempool::Key>
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
    MempoolNetAdapter: MempoolNetworkAdapter<RuntimeServiceId, Payload = Mempool::Item, Key = Mempool::Key>
        + Send
        + Sync
        + 'static,
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    MempoolNetAdapter::Settings: Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + Send + 'static,
    SamplingNetworkAdapter:
        nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId> + Send + Sync,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId> + Send + Sync,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Self>
        + AsServiceId<Cryptarchia>
        + AsServiceId<NetworkService<NetAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<MempoolNetAdapter, Mempool, Mempool::Storage, RuntimeServiceId>,
        >
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetworkAdapter,
                SamplingStorage,
                MempoolDaAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<TimeService<TimeBackend, RuntimeServiceId>>,
{
    fn init(
        service_resources_handle: OpaqueServiceResourcesHandle<Self, RuntimeServiceId>,
        _initial_state: Self::State,
    ) -> Result<Self, DynError> {
        Ok(Self {
            service_resources_handle,
        })
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn run(mut self) -> Result<(), DynError> {
        let relays: ChainNetworkRelays<
            Cryptarchia,
            Mempool,
            MempoolNetAdapter,
            MempoolDaAdapter,
            NetAdapter,
            SamplingBackend,
            RuntimeServiceId,
        > = ChainNetworkRelays::from_service_resources_handle::<_, _, _>(
            &self.service_resources_handle,
        )
        .await;

        let ChainNetworkSettings {
            config: ledger_config,
            network_adapter_settings,
            bootstrap: bootstrap_config,
            sync: sync_config,
        } = self
            .service_resources_handle
            .settings_handle
            .notifier()
            .get_updated_settings();

        let network_adapter =
            NetAdapter::new(network_adapter_settings, relays.network_relay().clone()).await;

        let mut incoming_proposals = network_adapter.proposals_stream().await?;
        let mut chainsync_events = network_adapter.chainsync_events_stream().await?;

        let mut orphan_downloader = Box::pin(OrphanBlocksDownloader::new(
            network_adapter.clone(),
            sync_config.orphan.max_orphan_cache_size,
        ));

        let recent_blob_validation = blob::Validation::<RecentBlobStrategy>::new(
            ledger_config.base_period_length(),
            relays.sampling_relay().clone(),
            relays.time_relay().clone(),
        );
        let historic_blob_validation = blob::Validation::<HistoricBlobStrategy>::new(
            ledger_config.base_period_length(),
            relays.sampling_relay().clone(),
            relays.time_relay().clone(),
        );

        wait_until_services_are_ready!(
            &self.service_resources_handle.overwatch_handle,
            Some(Duration::from_secs(60)),
            Cryptarchia,
            NetworkService<_, _>,
            TxMempoolService<_, _, _, _>,
            DaSamplingService<_, _, _, _, _>,
            TimeService<_, _>
        )
        .await?;

        let initial_block_download = InitialBlockDownload::new(
            bootstrap_config.ibd,
            ChainNetworkIbdBlockProcessor::<_, Mempool, SamplingBackend, _> {
                historic_blob_validation: historic_blob_validation.clone(),
                cryptarchia: relays.cryptarchia().clone(),
                mempool_adapter: relays.mempool_adapter().clone(),
                sampling_relay: relays.sampling_relay().clone(),
            },
            network_adapter,
        );

        match initial_block_download.run().await {
            Ok(_) => {
                info!("Initial Block Download completed successfully.");
                // Notify chain-service that IBD is complete so it can start the prolonged
                // bootstrap timer
                if let Err(e) = relays.cryptarchia().notify_ibd_completed().await {
                    error!("Failed to notify chain-service of IBD completion: {e:?}");
                }
            }
            Err(e) => {
                error!("Initial Block Download failed: {e:?}. Initiating graceful shutdown.");

                if let Err(shutdown_err) = self
                    .service_resources_handle
                    .overwatch_handle
                    .shutdown()
                    .await
                {
                    error!("Failed to shutdown overwatch: {shutdown_err:?}");
                }

                error!(
                    "Initial Block Download did not complete successfully: {e}. Common causes: unresponsive initial peers, \
                network issues, or incorrect peer addresses. Consider retrying with different bootstrap peers."
                );

                return Err(DynError::from(format!(
                    "Initial Block Download failed: {e:?}"
                )));
            }
        }

        self.notify_service_ready();

        let async_loop = async {
            loop {
                tokio::select! {
                    Some(proposal) = incoming_proposals.next() => {
                        self.handle_incoming_proposal(
                            proposal,
                            &recent_blob_validation,
                            orphan_downloader.as_mut().get_mut(),
                            &relays,
                        )
                        .await;
                    }

                    Some(event) = chainsync_events.next() => {
                        // Forward the chain sync event to chain-service for handling
                        if let Err(e) = relays.cryptarchia().handle_chainsync_event(event).await {
                            error!(target: LOG_TARGET, "Failed to forward chainsync event to chain-service: {e}");
                        }
                    }

                    Some(block) = orphan_downloader.next(), if orphan_downloader.should_poll() => {
                        let header_id = block.header().id();
                        info!("Processing block from orphan downloader: {header_id:?}");

                        if !should_process_block(relays.cryptarchia(), block.header().id()).await {
                            continue;
                        }

                        Self::log_received_block(&block);

                        match process_block::<_,_,Mempool, SamplingBackend,_>(
                            block.clone(),
                            Some(&historic_blob_validation),
                            relays.cryptarchia(),
                            relays.mempool_adapter(),
                            relays.sampling_relay(),
                        ).await {
                            Ok(()) => {
                                info!(counter.consensus_processed_blocks = 1);
                            }
                            Err(e) => {
                                error!(target: LOG_TARGET, "Error processing orphan downloader block: {e:?}");
                                orphan_downloader.cancel_active_download();
                            }
                        }
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

impl<
    Cryptarchia,
    NetAdapter,
    Mempool,
    MempoolNetAdapter,
    MempoolDaAdapter,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingStorage,
    TimeBackend,
    RuntimeServiceId,
>
    ChainNetwork<
        Cryptarchia,
        NetAdapter,
        Mempool,
        MempoolNetAdapter,
        MempoolDaAdapter,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
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
    NetAdapter::PeerId: Clone + Eq + Hash + Copy + Debug + Send + Sync,
    Mempool: RecoverableMempool<BlockId = HeaderId, Key = TxHash> + Send + Sync + 'static,
    Mempool::RecoveryState: Serialize + for<'de> Deserialize<'de>,
    Mempool::Settings: Clone + Send + Sync + 'static,
    Mempool::Storage: MempoolStorageAdapter<RuntimeServiceId> + Clone + Send + Sync,
    Mempool::Item: Transaction<Hash = Mempool::Key>
        + AuthenticatedMantleTx
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
    MempoolDaAdapter: DaMempoolAdapter + Send + Sync + 'static,
    MempoolNetAdapter::Settings: Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId> + Send,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    TimeBackend: nomos_time::backends::TimeBackend,
    TimeBackend::Settings: Clone + Send + Sync,
    RuntimeServiceId: Display + AsServiceId<Self> + Sync,
{
    fn notify_service_ready(&self) {
        self.service_resources_handle.status_updater.notify_ready();
        info!(
            "Service '{}' is ready.",
            <RuntimeServiceId as AsServiceId<Self>>::SERVICE_ID
        );
    }

    async fn handle_incoming_proposal(
        &self,
        proposal: Proposal,
        recent_blob_validation: &blob::Validation<RecentBlobStrategy>,
        orphan_downloader: &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
        relays: &ChainNetworkRelays<
            Cryptarchia,
            Mempool,
            MempoolNetAdapter,
            MempoolDaAdapter,
            NetAdapter,
            SamplingBackend,
            RuntimeServiceId,
        >,
    ) where
        RuntimeServiceId: Send + Sync + 'static,
    {
        let block_id = proposal.header().id();

        if !should_process_block(relays.cryptarchia(), block_id).await {
            info!(
                target: LOG_TARGET,
                "Block {block_id:?} already processed, ignoring"            );
            return;
        }

        let block = match reconstruct_block_from_proposal(proposal, relays.mempool_adapter()).await
        {
            Ok(block) => block,
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    "Failed to reconstruct block from proposal: {:?}",
                    e
                );
                return;
            }
        };

        self.apply_reconstructed_block(block, recent_blob_validation, orphan_downloader, relays)
            .await;
    }

    fn handle_proposal_processing_error(
        err: Error,
        block_id: HeaderId,
        orphan_downloader: &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
    ) where
        RuntimeServiceId: Send + Sync + 'static,
    {
        match err {
            Error::Cryptarchia(chain_service::api::ApiError::ParentMissing { parent, info }) => {
                orphan_downloader.enqueue_orphan(block_id, info.tip, info.lib);

                error!(
                    target: LOG_TARGET,
                    "Received block with parent {:?} that is not in the ledger state. Ignoring block.",
                    parent
                );
            }
            other => {
                error!(
                    target: LOG_TARGET,
                    "Error processing reconstructed block: {:?}",
                    other
                );
            }
        }
    }

    async fn apply_reconstructed_block(
        &self,
        block: Block<Mempool::Item>,
        recent_blob_validation: &blob::Validation<RecentBlobStrategy>,
        orphan_downloader: &mut OrphanBlocksDownloader<NetAdapter, RuntimeServiceId>,
        relays: &ChainNetworkRelays<
            Cryptarchia,
            Mempool,
            MempoolNetAdapter,
            MempoolDaAdapter,
            NetAdapter,
            SamplingBackend,
            RuntimeServiceId,
        >,
    ) where
        RuntimeServiceId: Send + Sync + 'static,
    {
        Self::log_received_block(&block);

        let block_id = block.header().id();

        match process_block::<_, _, Mempool, SamplingBackend, _>(
            block,
            Some(recent_blob_validation),
            relays.cryptarchia(),
            relays.mempool_adapter(),
            relays.sampling_relay(),
        )
        .await
        {
            Ok(()) => {
                orphan_downloader.remove_orphan(&block_id);
                info!(counter.consensus_processed_blocks = 1);
            }
            Err(err) => {
                Self::handle_proposal_processing_error(err, block_id, orphan_downloader);
            }
        }
    }

    fn log_received_block(block: &Block<Mempool::Item>) {
        let content_size = 0; // TODO: calculate the actual content size
        let transactions = block.transactions().len();

        info!(
            counter.received_blocks = 1,
            transactions = transactions,
            bytes = content_size
        );
        info!(
            histogram.received_blocks_data = content_size,
            transactions = transactions,
        );
    }
}

async fn should_process_block<Cryptarchia, RuntimeServiceId>(
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    block_id: HeaderId,
) -> bool
where
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: AuthenticatedMantleTx + Debug + Clone + Send + Sync,
    RuntimeServiceId: Send + Sync,
{
    match cryptarchia.get_ledger_state(block_id).await {
        Ok(Some(_)) => {
            info!(
                target: LOG_TARGET,
                "Block {:?} already processed, ignoring",
                block_id
            );
            false
        }
        Ok(None) => {
            // block has not been processed
            true
        }
        Err(err) => {
            error!(target: LOG_TARGET, err = ?err, "Failure when checking if block already processed");
            // block processing is idempotent, so we can safely re-process a block
            true
        }
    }
}

/// Try to add a [`Block`] to [`Cryptarchia`].
/// A [`Block`] is only added if it's valid
#[expect(clippy::allow_attributes_without_reason)]
#[instrument(
    level = "debug",
    skip(blob_validation, cryptarchia, mempool_adapter, sampling_relay)
)]
async fn process_block<BlobStrategy, Cryptarchia, Mempool, SamplingBackend, RuntimeServiceId>(
    block: Block<Cryptarchia::Tx>,
    blob_validation: Option<&blob::Validation<BlobStrategy>>,
    cryptarchia: &CryptarchiaServiceApi<Cryptarchia, RuntimeServiceId>,
    mempool_adapter: &MempoolAdapter<Mempool::Item, Mempool::Item>,
    sampling_relay: &SamplingRelay<SamplingBackend::BlobId>,
) -> Result<(), Error>
where
    BlobStrategy: blob::Strategy + Sync,
    Cryptarchia: CryptarchiaServiceData,
    Cryptarchia::Tx: AuthenticatedMantleTx + Debug + Clone + Send + Sync,
    Mempool:
        RecoverableMempool<BlockId = HeaderId, Key = TxHash, Item = Cryptarchia::Tx> + Send + Sync,
    SamplingBackend: DaSamplingServiceBackend<BlobId = da::BlobId>,
    RuntimeServiceId: Send + Sync,
{
    debug!("received proposal {:?}", block);

    let header = block.header();
    let id = header.id();

    if let Some(blob_validation) = blob_validation {
        blob_validation.validate(&block).await?;
    }

    cryptarchia.apply_block(block.clone()).await?;

    // remove included content from mempool
    mempool_adapter
        .mark_transactions_in_block(
            &block
                .transactions()
                .map(Transaction::hash)
                .collect::<Vec<_>>(),
            id,
        )
        .await
        .unwrap_or_else(|e| error!("Could not mark transactions in block: {e}"));

    let blob_ids: Vec<da::BlobId> = block
        .transactions()
        .flat_map(|tx| tx.mantle_tx().ops.iter())
        .filter_map(|op| {
            if let Op::ChannelBlob(blob_op) = op {
                Some(blob_op.blob)
            } else {
                None
            }
        })
        .collect();

    if !blob_ids.is_empty() {
        mark_blob_in_block(sampling_relay, blob_ids).await;
    }

    Ok(())
}

async fn mark_blob_in_block<BlobId: Debug + Send>(
    sampling_relay: &SamplingRelay<BlobId>,
    blobs_id: Vec<BlobId>,
) {
    if let Err((_e, DaSamplingServiceMsg::MarkInBlock { blobs_id })) = sampling_relay
        .send(DaSamplingServiceMsg::MarkInBlock { blobs_id })
        .await
    {
        error!("Error marking in block for blobs ids: {blobs_id:?}");
    }
}

/// Reconstruct a Block from a Proposal by looking up transactions from mempool
async fn reconstruct_block_from_proposal<Payload, Item>(
    proposal: Proposal,
    mempool: &MempoolAdapter<Payload, Item>,
) -> Result<Block<Item>, Error>
where
    Payload: Send + Sync,
    Item: AuthenticatedMantleTx<Hash = TxHash> + Clone + Send + Sync + 'static,
{
    let mempool_hashes: Vec<TxHash> = proposal.mempool_transactions().to_vec();
    let mempool_response = mempool
        .get_transactions_by_hashes(mempool_hashes)
        .await
        .map_err(|e| {
            Error::InvalidBlock(format!("Failed to get transactions from mempool: {e}"))
        })?;

    if !mempool_response.all_found() {
        return Err(Error::InvalidBlock(format!(
            "Failed to reconstruct block: {:?} mempool transactions not found",
            mempool_response.not_found()
        )));
    }

    let reconstructed_transactions = mempool_response.into_found();

    let header = proposal.header().clone();
    let signature = *proposal.signature();

    let block = Block::reconstruct(header, reconstructed_transactions, signature)
        .map_err(|e| Error::InvalidBlock(format!("Invalid block: {e}")))?;

    Ok(block)
}
