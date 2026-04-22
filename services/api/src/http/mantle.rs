use core::fmt::Debug;
use std::{collections::BTreeSet, fmt::Display, num::NonZeroUsize, ops::RangeInclusive};

use bytes::Bytes;
use futures::{Stream, StreamExt as _, future::join_all};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockBroadcastService, BlockInfo};
use lb_chain_service::{
    ConsensusMsg, ProcessedBlockEvent, Slot,
    storage::{StorageAdapter as _, adapters::StorageAdapter},
};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction, TxHash, channel::ChannelState, ops::channel::ChannelId},
    sdp::Declaration,
};
use lb_storage_service::{
    StorageMsg, StorageService,
    api::{
        StorageApiRequest,
        chain::{StorageChainApi, requests::ChainApiRequest},
    },
};
use lb_tx_service::{
    MempoolMetrics, MempoolMsg, TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
    tx::service::openapi::Status,
};
use overwatch::services::{AsServiceId, ServiceData};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::oneshot;
use tokio_stream::wrappers::BroadcastStream;

use crate::http::consensus::{Cryptarchia, cryptarchia_ledger_state};

/// A block along with the current chain state (tip and LIB) at the time it was
/// processed. This allows clients to track the canonical chain without needing
/// to poll /cryptarchia/info.
pub struct BlockWithChainState<Tx> {
    /// The processed block.
    pub block: Block<Tx>,
    /// The current canonical tip after processing this block.
    pub tip: HeaderId,
    pub tip_slot: Slot,
    /// The current Last Irreversible Block after processing this block.
    pub lib: HeaderId,
    pub lib_slot: Slot,
}

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

pub async fn channel<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    id: ChannelId,
) -> Result<ChannelState, super::DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let ledger_state = cryptarchia_ledger_state(handle).await?;
    ledger_state
        .mantle_ledger()
        .channels()
        .channel_state(&id)
        .cloned()
        .ok_or_else(|| "channel not found".into())
}

pub async fn mantle_mempool_metrics<StorageAdapter, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<MempoolMetrics, super::DynError>
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
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
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
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

pub async fn get_processed_blocks_event_stream<Transaction, Service, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<
    impl Stream<Item = Result<ProcessedBlockEvent, crate::http::DynError>>
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

    let processed_blocks_stream = BroadcastStream::new(new_blocks_receiver)
        .map(|item| item.map_err(|error| Box::new(error) as crate::http::DynError));

    Ok(processed_blocks_stream)
}

pub async fn get_new_blocks_stream<
    Transaction,
    StorageBackend,
    ConsensusService,
    RuntimeServiceId,
>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<
    impl Stream<Item = BlockWithChainState<Transaction>>
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
        + lb_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
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
    let processed_blocks_stream =
        get_processed_blocks_event_stream::<Transaction, ConsensusService, RuntimeServiceId>(
            handle,
        )
        .await?;

    let relay = handle
        .relay::<StorageService<StorageBackend, RuntimeServiceId>>()
        .await?;
    let storage_adapter =
        StorageAdapter::<StorageBackend, Transaction, RuntimeServiceId>::new(relay).await;

    let new_blocks_stream = processed_blocks_stream.filter_map(move |event| {
        let storage_adapter = storage_adapter.clone();
        async move {
            let event = event.ok()?;
            let block = storage_adapter.get_block(&event.block_id).await?;
            Some(BlockWithChainState {
                block,
                tip: event.tip,
                tip_slot: event.tip_slot,
                lib: event.lib,
                lib_slot: event.lib_slot,
            })
        }
    });

    Ok(new_blocks_stream)
}

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
    Backend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
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
        + lb_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
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

/// Fetch a single block by its header ID.
///
/// # Arguments
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `header_id`: The `HeaderId` of the block to fetch.
///
/// # Returns
///
/// If successful, returns `Some(Block<Transaction>)` if the block exists, or
/// `None` if no block with the given header ID was found. Returns a boxed
/// `DynError` if any error occurs during processing.
pub async fn get_block<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    header_id: HeaderId,
) -> Result<Option<Block<Transaction>>, super::DynError>
where
    Transaction: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + lb_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<Transaction>> + TryInto<Block<Transaction>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let storage_adapter = StorageAdapter::<_, _, RuntimeServiceId>::new(relay).await;
    Ok(storage_adapter.get_block(&header_id).await)
}

/// Fetch transactions by their hashes.
///
/// # Arguments
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `tx_hashes`: The set of [`TxHash`]es to fetch.
///
/// # Returns
///
/// If successful, returns a stream of matching [`Transaction`]s.
/// Returns a boxed `DynError` if any error occurs during processing.
pub async fn get_transactions<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    tx_hashes: BTreeSet<TxHash>,
) -> Result<
    impl Stream<Item = Transaction> + use<Transaction, StorageBackend, RuntimeServiceId>,
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
        + lb_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<Transaction>> + TryInto<Block<Transaction>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let storage_adapter = StorageAdapter::<_, _, RuntimeServiceId>::new(relay).await;
    storage_adapter.get_transactions(tx_hashes).await
}

/// Fetch a single transaction by its hash.
///
/// # Arguments
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `tx_hash`: The [`TxHash`] of the transaction to fetch.
///
/// # Returns
///
/// - `Ok(Some(tx))`: Found transaction.
/// - `Ok(None)`: No transaction with the given hash was found.
/// - `Err(_)`: An error occurred during processing.
pub async fn get_transaction<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    tx_hash: TxHash,
) -> Result<Option<Transaction>, super::DynError>
where
    Transaction: Clone
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static
        + lb_core::mantle::Transaction<Hash = TxHash>,
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<Transaction>> + TryInto<Block<Transaction>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let mut stream = get_transactions::<Transaction, StorageBackend, RuntimeServiceId>(
        handle,
        BTreeSet::from([tx_hash]),
    )
    .await?;

    // Assume only one transaction is returned
    Ok(stream.next().await)
}

pub async fn get_sdp_declarations<RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
) -> Result<Vec<Declaration>, super::DynError>
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let relay = handle.relay::<Cryptarchia<RuntimeServiceId>>().await?;
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
