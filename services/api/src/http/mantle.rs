use core::fmt::Debug;
use std::{collections::BTreeSet, fmt::Display, num::NonZeroUsize, ops::RangeInclusive};

use bytes::Bytes;
use futures::{Stream, StreamExt as _, future::join_all};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockBroadcastService, BlockInfo};
use lb_chain_service::{
    ConsensusMsg, CryptarchiaInfo, ProcessedBlockEvent, Slot,
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
use tracing::warn;

use crate::http::{
    consensus::{Cryptarchia, cryptarchia_ledger_state},
    errors::BlockSlotRangeError,
};

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

async fn get_immutable_block_ids_in_slot_range<Backend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    slot_from: Slot,
    slot_to: Slot,
    limit: NonZeroUsize,
    descending: bool,
) -> Result<Vec<HeaderId>, super::DynError>
where
    Backend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<StorageService<Backend, RuntimeServiceId>>,
{
    let relay = handle.relay().await?;
    let (response_tx, response_rx) = oneshot::channel();
    let request = if descending {
        ChainApiRequest::ScanImmutableBlockIdsReverse {
            slot_range: RangeInclusive::new(slot_from, slot_to),
            limit,
            response_tx,
        }
    } else {
        ChainApiRequest::ScanImmutableBlockIds {
            slot_range: RangeInclusive::new(slot_from, slot_to),
            limit,
            response_tx,
        }
    };

    relay
        .send(StorageMsg::Api {
            request: StorageApiRequest::Chain(request),
        })
        .await
        .map_err(|(error, _)| error)?;

    response_rx
        .await
        .map_err(|error| Box::new(error) as super::DynError)
}

async fn load_blocks_with_chain_state_by_ids<Transaction, StorageBackend, RuntimeServiceId>(
    storage_adapter: &StorageAdapter<StorageBackend, Transaction, RuntimeServiceId>,
    header_ids: Vec<HeaderId>,
    chain_info: &CryptarchiaInfo,
    blocks_limit: usize,
) -> Result<Vec<BlockWithChainState<Transaction>>, super::DynError>
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
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let mut blocks = Vec::with_capacity(header_ids.len().min(blocks_limit));
    for header_id in header_ids {
        let Some(block) = storage_adapter.get_block(&header_id).await else {
            warn!("missing block body for indexed header {header_id}, skipping");
            continue;
        };
        blocks.push(BlockWithChainState {
            block,
            tip: chain_info.tip,
            tip_slot: chain_info.slot,
            lib: chain_info.lib,
            lib_slot: chain_info.lib_slot,
        });
        if blocks.len() == blocks_limit {
            break;
        }
    }

    Ok(blocks)
}

fn validate_blocks_slot_range(
    slot_from: Slot,
    slot_to: Slot,
    immutable_only: bool,
    chain_info: &CryptarchiaInfo,
) -> Result<(), BlockSlotRangeError> {
    if slot_to < slot_from {
        return Err(BlockSlotRangeError::InvalidRange { slot_from, slot_to });
    }
    if immutable_only {
        if slot_to > chain_info.lib_slot {
            return Err(BlockSlotRangeError::SlotToExceedsLibSlot {
                slot_to,
                lib_slot: chain_info.lib_slot,
            });
        }
    } else if slot_to > chain_info.slot {
        return Err(BlockSlotRangeError::SlotToExceedsTipSlot {
            slot_to,
            tip_slot: chain_info.slot,
        });
    }

    Ok(())
}

async fn fetch_and_load_mutable_blocks<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    storage_adapter: &StorageAdapter<StorageBackend, Transaction, RuntimeServiceId>,
    chain_info: &CryptarchiaInfo,
    slot_from: Slot,
    slot_to: Slot,
    remaining: usize,
    descending: bool,
) -> Result<Vec<BlockWithChainState<Transaction>>, super::DynError>
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
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>
        + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let limit = remaining;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut blocks = Vec::with_capacity(limit.min(1024));
    let mut current_id = chain_info.tip;
    let mut retried = false;

    loop {
        let Some(block) = storage_adapter.get_block(&current_id).await else {
            if retried {
                return Err(format!(
                    "canonical chain inconsistency: missing block for canonical header {current_id}"
                )
                .into());
            }

            let refreshed_info =
                crate::http::consensus::cryptarchia_info::<RuntimeServiceId>(handle).await?;
            current_id = refreshed_info.cryptarchia_info.tip;
            retried = true;
            blocks.clear();
            continue;
        };

        let header = block.header();
        let slot = header.slot();
        let parent_id = header.parent_block();

        if slot < slot_from {
            break;
        }

        if slot <= slot_to {
            blocks.push(BlockWithChainState {
                block,
                tip: chain_info.tip,
                tip_slot: chain_info.slot,
                lib: chain_info.lib,
                lib_slot: chain_info.lib_slot,
            });

            if descending && blocks.len() == limit {
                break;
            }
        }

        if parent_id == current_id {
            break;
        }
        current_id = parent_id;
    }

    if !descending {
        blocks.reverse();
        blocks.truncate(limit);
    }

    Ok(blocks)
}

async fn fetch_and_load_immutable_blocks<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    storage_adapter: &StorageAdapter<StorageBackend, Transaction, RuntimeServiceId>,
    chain_info: &CryptarchiaInfo,
    slot_from: Slot,
    immutable_slot_to: Slot,
    remaining: usize,
    descending: bool,
) -> Result<Vec<BlockWithChainState<Transaction>>, super::DynError>
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
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let limit = NonZeroUsize::new(remaining.saturating_mul(2).max(1))
        .expect("remaining is positive while fetching immutable blocks");
    let header_ids = get_immutable_block_ids_in_slot_range::<StorageBackend, RuntimeServiceId>(
        handle,
        slot_from,
        immutable_slot_to,
        limit,
        descending,
    )
    .await?;

    load_blocks_with_chain_state_by_ids(storage_adapter, header_ids, chain_info, remaining).await
}

pub async fn get_blocks_in_slot_range_with_snapshot<Transaction, StorageBackend, RuntimeServiceId>(
    handle: &overwatch::overwatch::handle::OverwatchHandle<RuntimeServiceId>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    blocks_limit: NonZeroUsize,
    immutable_only: bool,
    chain_info: &CryptarchiaInfo,
) -> Result<Vec<BlockWithChainState<Transaction>>, super::DynError>
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
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>
        + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    validate_blocks_slot_range(slot_from, slot_to, immutable_only, chain_info)
        .map_err(|e| Box::new(e) as super::DynError)?;

    let relay = handle
        .relay::<StorageService<StorageBackend, RuntimeServiceId>>()
        .await?;
    let storage_adapter =
        StorageAdapter::<StorageBackend, Transaction, RuntimeServiceId>::new(relay).await;

    let mut blocks = Vec::with_capacity(blocks_limit.get().min(1024));
    let mut remaining = blocks_limit.get();

    let immutable_slot_to = slot_to.min(chain_info.lib_slot);
    let has_immutable_range = slot_from <= immutable_slot_to;
    // Mutable window starts strictly above LIB; LIB itself is served via immutable
    // index.
    let mutable_slot_from = (chain_info.lib_slot + 1).max(slot_from);
    let has_mutable_range = !immutable_only && mutable_slot_from <= slot_to;

    let fetch_mutable = |remaining: usize, descending: bool| {
        let storage_adapter = storage_adapter.clone();
        async move {
            fetch_and_load_mutable_blocks::<Transaction, StorageBackend, RuntimeServiceId>(
                handle,
                &storage_adapter,
                chain_info,
                mutable_slot_from,
                slot_to,
                remaining,
                descending,
            )
            .await
        }
    };

    let fetch_immutable = |remaining: usize, descending: bool| {
        let storage_adapter = storage_adapter.clone();
        async move {
            fetch_and_load_immutable_blocks::<Transaction, StorageBackend, RuntimeServiceId>(
                handle,
                &storage_adapter,
                chain_info,
                slot_from,
                immutable_slot_to,
                remaining,
                descending,
            )
            .await
        }
    };

    if descending {
        if remaining > 0 && has_mutable_range {
            let mutable_blocks = fetch_mutable(remaining, true).await?;
            remaining = remaining.saturating_sub(mutable_blocks.len());
            blocks.extend(mutable_blocks);
        }

        if remaining > 0 && has_immutable_range {
            let immutable_blocks = fetch_immutable(remaining, true).await?;
            blocks.extend(immutable_blocks);
        }
    } else {
        if remaining > 0 && has_immutable_range {
            let immutable_blocks = fetch_immutable(remaining, false).await?;
            remaining = remaining.saturating_sub(immutable_blocks.len());
            blocks.extend(immutable_blocks);
        }

        if remaining > 0 && has_mutable_range {
            let mutable_blocks = fetch_mutable(remaining, false).await?;
            blocks.extend(mutable_blocks);
        }
    }

    Ok(blocks)
}

/// Fetch immutable block header ids in range.
///
/// # Arguments
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `from_slot`: A non-zero starting slot (inclusive) indicating the starting
///   point of the desired slot range.
/// - `to_slot`: A non-zero ending slot (inclusive) indicating the endpoint of
///   the desired slot range. If the range spans across the LIB block, only
///   header IDs up to LIB will be returned.
///
/// # Returns
///
/// If successful, returns a `Vec<HeaderId>` containing the block header IDs for
/// the specified slot range. If any error occurs during processing, returns a
/// boxed `DynError`.
pub async fn get_immutable_blocks_header_ids<Backend, RuntimeServiceId>(
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

/// Fetch immutable blocks in range
///
/// # Parameters
///
/// - `handle`: A reference to the `OverwatchHandle` to interact with the
///   runtime and storage service.
/// - `from_slot`: A non-zero starting slot (inclusive) indicating the starting
///   point of the desired slot range.
/// - `to_slot`: A non-zero ending slot (inclusive) indicating the endpoint of
///   the desired slot range. If the range spans across the LIB block, only
///   blocks up to LIB will be returned.
///
/// # Returns
///
/// If successful, returns a `Vec` containing the immutable blocks for the
/// specified slot range. If any error occurs during processing, returns a boxed
/// `DynError`.
pub async fn get_immutable_blocks<Transaction, StorageBackend, RuntimeServiceId>(
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
    let header_ids = get_immutable_blocks_header_ids(handle, from_slot, to_slot).await?;

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
        .send(ConsensusMsg::GetSdpDeclarations {
            reply_channel: sender,
        })
        .await
        .map_err(|(e, _)| e)?;

    let declarations = receiver
        .await?
        .into_iter()
        .map(|(_, declaration)| declaration)
        .collect();

    Ok(declarations)
}
