use core::fmt::Debug;
use std::fmt::Display;

use broadcast_service::{BlockBroadcastMsg, BlockBroadcastService, BlockInfo};
use chain_service::ConsensusMsg;
use futures::{Stream, StreamExt as _};
use nomos_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction},
    sdp::Declaration,
};
use overwatch::services::AsServiceId;
use tokio::sync::oneshot;
use tokio_stream::wrappers::BroadcastStream;
use tx_service::{
    MempoolMetrics, MempoolMsg, TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
    tx::service::openapi::Status,
};
#[cfg(feature = "block-explorer")]
use {
    bytes::Bytes,
    chain_service::Slot,
    chain_service::storage::StorageAdapter as _,
    chain_service::storage::adapters::StorageAdapter,
    futures::future::join_all,
    nomos_core::{block::Block, mantle::TxHash},
    nomos_storage::{
        StorageMsg, StorageService,
        api::{
            StorageApiRequest,
            chain::{StorageChainApi, requests::ChainApiRequest},
        },
    },
    overwatch::services::ServiceData,
    serde::{Serialize, de::DeserializeOwned},
    std::{num::NonZeroUsize, ops::RangeInclusive},
};

pub type MempoolService<StorageAdapter, RuntimeServiceId> = TxMempoolService<
    MempoolNetworkAdapter<SignedMantleTx, <SignedMantleTx as Transaction>::Hash, RuntimeServiceId>,
    Mempool<
        HeaderId,
        SignedMantleTx,
        <SignedMantleTx as Transaction>::Hash,
        StorageAdapter,
        RuntimeServiceId,
    >,
    StorageAdapter,
    RuntimeServiceId,
>;

pub async fn mantle_mempool_metrics<StorageAdapter, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<MempoolMetrics, super::DynError>
where
    StorageAdapter: tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Key = <SignedMantleTx as Transaction>::Hash,
            Item = SignedMantleTx,
        > + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + AsServiceId<MempoolService<StorageAdapter, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(MempoolMsg::Metrics {
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver.await.map_err(|e| Box::new(e) as super::DynError)
}

pub async fn mantle_mempool_status<StorageAdapter, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    items: Vec<<SignedMantleTx as Transaction>::Hash>,
) -> Result<Vec<Status>, super::DynError>
where
    StorageAdapter: tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Key = <SignedMantleTx as Transaction>::Hash,
            Item = SignedMantleTx,
        > + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + AsServiceId<MempoolService<StorageAdapter, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(MempoolMsg::Status {
            items,
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    receiver.await.map_err(|e| Box::new(e) as super::DynError)
}

pub async fn lib_block_stream<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<
    impl Stream<Item = Result<BlockInfo, crate::http::DynError>> + Send + Sync + use<RuntimeServiceId>,
    super::DynError,
>
where
    RuntimeServiceId: Debug + Sync + Display + AsServiceId<BlockBroadcastService<RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();
    relay
        .send(BlockBroadcastMsg::SubscribeToFinalizedBlocks {
            result_sender: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    let broadcast_receiver = receiver.await.map_err(|e| Box::new(e) as super::DynError)?;
    let stream = BroadcastStream::new(broadcast_receiver)
        .map(|result| result.map_err(|e| Box::new(e) as crate::http::DynError));

    Ok(stream)
}

#[cfg(feature = "block-explorer")]
pub async fn get_new_header_ids_stream<Transaction, Service, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<
    impl Stream<Item = Result<HeaderId, crate::http::DynError>>
    + Send
    + Sync
    + use<Transaction, Service, RuntimeServiceId>,
    super::DynError,
>
where
    Transaction: Send + 'static,
    Service: ServiceData<Message = ConsensusMsg<Transaction>>,
    RuntimeServiceId: Debug + Sync + Display + AsServiceId<Service>,
{
    let relay = handle.relay().await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ConsensusMsg::NewBlockSubscribe { sender })
        .await
        .map_err(|(error, _)| error)?;

    let new_blocks_receiver = receiver
        .await
        .map_err(|error| Box::new(error) as super::DynError)?;

    let new_header_ids_stream = BroadcastStream::new(new_blocks_receiver)
        .map(|item| item.map_err(|error| Box::new(error) as crate::http::DynError));

    Ok(new_header_ids_stream)
}

#[cfg(feature = "block-explorer")]
pub async fn get_new_blocks_stream<
    Transaction,
    StorageBackend,
    ConsensusService,
    RuntimeServiceId,
>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<
    impl Stream<Item = Block<Transaction>>
    + Send
    + use<Transaction, StorageBackend, ConsensusService, RuntimeServiceId>,
    super::DynError,
>
where
    Transaction: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + nomos_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: nomos_storage::backends::StorageBackend + Send + Sync + 'static,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<Transaction>> + TryInto<Block<Transaction>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    ConsensusService: ServiceData<Message = ConsensusMsg<Transaction>>,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>
        + AsServiceId<ConsensusService>,
{
    let new_header_ids_stream =
        get_new_header_ids_stream::<Transaction, ConsensusService, RuntimeServiceId>(handle)
            .await?;

    let relay = handle
        .relay::<StorageService<StorageBackend, RuntimeServiceId>>()
        .await?;
    let storage_adapter =
        StorageAdapter::<StorageBackend, Transaction, RuntimeServiceId>::new(relay).await;

    let new_blocks_stream = new_header_ids_stream.filter_map(move |header_id| {
        let storage_adapter = storage_adapter.clone();
        async move {
            let header_id = header_id.ok()?;
            storage_adapter.get_block(&header_id).await
        }
    });

    Ok(new_blocks_stream)
}

#[cfg(feature = "block-explorer")]
/// Fetch block header ids in range.
///
/// # Arguments
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `from_slot`: A non-zero starting slot (inclusive) indicating the starting
///   point of the desired slot range.
/// - `to_slot`: A non-zero ending slot (inclusive) indicating the endpoint of
///   the desired slot range.
///
/// # Returns
///
/// If successful, returns a `Vec<HeaderId>` containing the block header IDs for
/// the specified slot range. If any error occurs during processing, returns a
/// boxed `DynError`.
pub async fn get_blocks_header_ids<Backend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    from_slot: usize,
    to_slot: usize,
) -> Result<Vec<HeaderId>, super::DynError>
where
    Backend: nomos_storage::backends::StorageBackend + Send + Sync + 'static,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<Backend, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (response_tx, response_rx) = oneshot::channel();

    let limit = {
        // Since this request requires a limit, let's calculate it based on the slot
        // range. Add 1 to the difference to ensure that limit makes sense.
        let diff = to_slot - from_slot + 1;
        NonZeroUsize::new(diff)
            .ok_or_else(|| String::from("to_slot must be greater or equal to from_slot"))
    }?;

    let start = Slot::new(from_slot as u64);
    let end = Slot::new(to_slot as u64);
    let slot_range = RangeInclusive::new(start, end);

    relay
        .send(StorageMsg::Api {
            request: StorageApiRequest::Chain(ChainApiRequest::ScanImmutableBlockIds {
                slot_range,
                limit,
                response_tx,
            }),
        })
        .await
        .map_err(|(error, _)| error)?;

    response_rx
        .await
        .map_err(|error| Box::new(error) as super::DynError)
}

#[cfg(feature = "block-explorer")]
/// Fetch blocks in range
///
/// # Parameters
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `from_slot`: A non-zero starting slot (inclusive) indicating the starting
///   point of the desired slot range.
/// - `to_slot`: A non-zero ending slot (inclusive) indicating the endpoint of
///   the desired slot range.
///
/// # Returns
///
/// If successful, returns a `Vec` containing the blocks for the specified slot
/// range. If any error occurs during processing, returns a boxed `DynError`.
pub async fn get_blocks<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    from_slot: usize,
    to_slot: usize,
) -> Result<Vec<Block<Transaction>>, super::DynError>
where
    Transaction: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + nomos_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: nomos_storage::backends::StorageBackend + Send + Sync + 'static,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<Transaction>> + TryInto<Block<Transaction>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let header_ids = get_blocks_header_ids(handle, from_slot, to_slot).await?;

    let relay = handle.relay().await?;
    let storage_adapter = StorageAdapter::<_, _, RuntimeServiceId>::new(relay).await;

    let blocks_futures = header_ids
        .iter()
        .map(|header_id| storage_adapter.get_block(header_id));

    let blocks = join_all(blocks_futures)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    Ok(blocks)
}

pub async fn get_sdp_declarations<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Vec<Declaration>, super::DynError>
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<super::consensus::Cryptarchia<RuntimeServiceId>>,
{
    let relay = handle
        .relay::<super::consensus::Cryptarchia<RuntimeServiceId>>()
        .await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(ConsensusMsg::GetSdpDeclarations { tx: sender })
        .await
        .map_err(|(e, _)| e)?;

    let declarations = receiver
        .await?
        .into_iter()
        .map(|(_, declaration)| declaration)
        .collect();

    Ok(declarations)
}
