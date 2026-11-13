use std::{fmt::Display, num::NonZeroUsize};

use futures::{Stream, StreamExt as _, TryStreamExt as _};
use lb_chain_broadcast_service::{BlockBroadcastMsg, BlockInfo};
use lb_chain_service::{CryptarchiaInfo, ProcessedBlockEvent, Slot};
use lb_core::{
    block::Block,
    events::Events,
    header::HeaderId,
    mantle::{
        traits::{Hashable, StorageSize, hashable},
        transactions::hash::TxHash,
    },
};
use lb_log_targets::api;
use lb_storage_service::api::StorageApi;
use serde::Serialize;
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;

use super::{
    DynError, Error, Handle,
    cryptarchia::{self, chain_api},
    request,
};
use crate::{BlockBroadcastService, UnvalidatedTx};

const LOG_TARGET: &str = api::http::MANTLE;

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

pub async fn lib_block_stream(
    handle: &Handle,
) -> Result<impl Stream<Item = Result<BlockInfo, DynError>> + Send + Sync + use<>, Error> {
    let broadcast_receiver = request::<BlockBroadcastService, _>(handle, |result_sender| {
        BlockBroadcastMsg::SubscribeToFinalizedBlocks { result_sender }
    })
    .await?;
    let stream = BroadcastStream::new(broadcast_receiver)
        .map(|result| result.map_err(|e| Box::new(e) as DynError));

    Ok(stream)
}

/// A transaction with its hash beside it, as `id`.
#[derive(Serialize)]
#[serde(rename = "SignedMantleTx")]
#[derive(Clone)]
pub struct TxWithId {
    id: TxHash,
    #[serde(flatten)]
    tx: UnvalidatedTx,
}

impl Hashable for TxWithId {
    //noinspection RsTypeCheck: The type is correct, but the linter is confused by
    // the closure.
    const HASHER: hashable::Hasher<Self> = |tx| <UnvalidatedTx as Hashable>::HASHER(&tx.tx);
    type Hash = <UnvalidatedTx as Hashable>::Hash;

    fn as_signing(&self) -> Vec<u8> {
        self.tx.as_signing()
    }
}

impl StorageSize for TxWithId {
    fn storage_size(&self) -> usize {
        self.tx.storage_size()
    }
}

/// The blocks the node processes, forks included, each transaction carrying
/// its hash as `id`.
pub async fn get_new_blocks_with_tx_ids_stream(
    handle: &Handle,
) -> Result<impl Stream<Item = Block<TxWithId>> + Send + use<>, Error> {
    Ok(get_new_blocks_stream(handle).await?.map(|event| {
        let block = event.block;
        let transactions = block.transactions().map_ref(|tx| TxWithId {
            id: tx.hash(),
            tx: tx.clone(),
        });
        Block::reconstruct(
            block.header().clone(),
            block.uncle_headers().clone(),
            transactions,
            *block.signature(),
            u32::MAX,
        )
        .expect("Block should always build from valid block")
    }))
}

async fn get_processed_blocks_event_stream(
    handle: &Handle,
) -> Result<impl Stream<Item = Result<ProcessedBlockEvent, DynError>> + Send + Sync + use<>, Error>
{
    let new_blocks_receiver = chain_api(handle).await.subscribe_new_blocks().await?;

    let processed_blocks_stream = BroadcastStream::new(new_blocks_receiver)
        .map(|item| item.map_err(|error| Box::new(error) as DynError));

    Ok(processed_blocks_stream)
}

pub async fn get_new_blocks_stream(
    handle: &Handle,
) -> Result<impl Stream<Item = BlockWithChainState<UnvalidatedTx>> + Send + use<>, Error> {
    let processed_blocks_stream = get_processed_blocks_event_stream(handle).await?;

    let storage = StorageApi::<UnvalidatedTx>::from_overwatch_handle(handle).await?;

    let new_blocks_stream = processed_blocks_stream.filter_map(move |event| {
        let storage = storage.clone();
        async move {
            let event = event.ok()?;
            let block = storage.get_block(&event.block_id).await?;
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

async fn get_immutable_block_ids_in_slot_range(
    handle: &Handle,
    slot_from: Slot,
    slot_to: Slot,
    limit: NonZeroUsize,
    descending: bool,
) -> Result<Vec<HeaderId>, Error> {
    StorageApi::<()>::from_overwatch_handle(handle)
        .await?
        .scan_immutable_block_ids(slot_from..=slot_to, limit, descending)
        .await
        .map_err(Error::Internal)
}

async fn load_blocks_with_chain_state_by_ids(
    storage: &StorageApi<UnvalidatedTx>,
    header_ids: Vec<HeaderId>,
    chain_info: &CryptarchiaInfo,
    blocks_limit: usize,
) -> Result<Vec<BlockWithChainState<UnvalidatedTx>>, Error> {
    let mut blocks = Vec::with_capacity(header_ids.len().min(blocks_limit));
    for header_id in header_ids {
        let Some(block) = storage.get_block(&header_id).await else {
            warn!(
                target: LOG_TARGET,
                "missing block body for indexed header {header_id}, skipping"
            );
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

async fn fetch_and_load_mutable_blocks(
    handle: &Handle,
    storage: &StorageApi<UnvalidatedTx>,
    chain_info: &CryptarchiaInfo,
    slot_from: Slot,
    slot_to: Slot,
    remaining: usize,
    descending: bool,
) -> Result<Vec<BlockWithChainState<UnvalidatedTx>>, Error> {
    let limit = remaining;
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut blocks = Vec::with_capacity(limit.min(1024));
    let mut current_id = chain_info.tip;
    let gated_slot_from = slot_from.max(chain_info.lib_slot.strict_add(1.into()));
    if gated_slot_from > slot_to {
        return Ok(Vec::new());
    }
    let mut retried = false;

    loop {
        // This function only serves the mutable window. Once we hit LIB we are below
        // the requested mutable range and should stop without loading the body.
        if current_id == chain_info.lib {
            break;
        }

        let Some(block) = storage.get_block(&current_id).await else {
            if retried {
                return Err(Error::internal(format!(
                    "canonical chain inconsistency: missing block {current_id} while traversing \
                    mutable chain anchored at LIB {}",
                    chain_info.lib
                )));
            }

            // Retry once from the latest tip if the original tip is not yet available.
            // The original LIB remains the anchor for this request.
            let refreshed_info = cryptarchia::info(handle).await?;
            current_id = refreshed_info.cryptarchia_info.tip;
            retried = true;
            blocks.clear();
            continue;
        };

        let header = block.header();
        let slot = header.slot();
        let parent_id = header.parent_block();

        if slot < gated_slot_from {
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

        // Defensive guard against malformed/self-parenting headers.
        if parent_id == current_id {
            return Err(Error::internal(format!(
                "canonical chain inconsistency: block {current_id} at slot {slot:?} is its own\
                 parent before anchored LIB {}",
                chain_info.lib
            )));
        }
        current_id = parent_id;
    }

    if !descending {
        blocks.reverse();
        blocks.truncate(limit);
    }

    Ok(blocks)
}

async fn fetch_and_load_immutable_blocks(
    handle: &Handle,
    storage: &StorageApi<UnvalidatedTx>,
    chain_info: &CryptarchiaInfo,
    slot_from: Slot,
    immutable_slot_to: Slot,
    remaining: usize,
    descending: bool,
) -> Result<Vec<BlockWithChainState<UnvalidatedTx>>, Error> {
    let limit = NonZeroUsize::new(remaining.saturating_mul(2).max(1))
        .expect("remaining is positive while fetching immutable blocks");
    let header_ids = get_immutable_block_ids_in_slot_range(
        handle,
        slot_from,
        immutable_slot_to,
        limit,
        descending,
    )
    .await?;

    load_blocks_with_chain_state_by_ids(storage, header_ids, chain_info, remaining).await
}

pub(super) async fn get_blocks_in_slot_range_with_snapshot(
    handle: &Handle,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    blocks_limit: NonZeroUsize,
    immutable_only: bool,
    chain_info: &CryptarchiaInfo,
) -> Result<Vec<BlockWithChainState<UnvalidatedTx>>, Error> {
    validate_blocks_slot_range(slot_from, slot_to, immutable_only, chain_info)
        .map_err(|error| Error::InvalidInput(error.to_string()))?;

    let storage = StorageApi::<UnvalidatedTx>::from_overwatch_handle(handle).await?;

    let mut blocks = Vec::with_capacity(blocks_limit.get().min(1024));
    let mut remaining = blocks_limit.get();

    let immutable_slot_to = slot_to.min(chain_info.lib_slot);
    let has_immutable_range = slot_from <= immutable_slot_to;
    // Mutable window starts strictly above LIB; LIB itself is served via immutable
    // index.
    let mutable_slot_from = (chain_info.lib_slot.strict_add(1.into())).max(slot_from);
    let has_mutable_range = !immutable_only && mutable_slot_from <= slot_to;

    let fetch_mutable = |remaining: usize, descending: bool| {
        let storage = storage.clone();
        async move {
            fetch_and_load_mutable_blocks(
                handle,
                &storage,
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
        let storage = storage.clone();
        async move {
            fetch_and_load_immutable_blocks(
                handle,
                &storage,
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

fn slot_range_limit(slot_from: Slot, slot_to: Slot) -> Option<NonZeroUsize> {
    slot_to
        .into_inner()
        .checked_sub(slot_from.into_inner())
        .and_then(|diff| diff.checked_add(1))
        .and_then(|diff| usize::try_from(diff).ok())
        .and_then(NonZeroUsize::new)
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
/// specified slot range.
pub async fn get_immutable_blocks(
    handle: &Handle,
    from_slot: usize,
    to_slot: usize,
) -> Result<Vec<Block<UnvalidatedTx>>, Error> {
    if to_slot < from_slot {
        return Err(Error::InvalidInput(
            "to_slot must be greater or equal to from_slot".into(),
        ));
    }

    let chain_info = cryptarchia::info(handle).await?.cryptarchia_info;
    let slot_from = Slot::new(from_slot as u64);
    if slot_from > chain_info.lib_slot {
        return Ok(Vec::new());
    }

    let slot_to = Slot::new(to_slot as u64).min(chain_info.lib_slot);
    let blocks_limit = slot_range_limit(slot_from, slot_to)
        .ok_or_else(|| Error::InvalidInput("legacy immutable block range is too large".into()))?;
    let blocks = get_blocks_in_slot_range_with_snapshot(
        handle,
        slot_from,
        slot_to,
        false,
        blocks_limit,
        true,
        &chain_info,
    )
    .await?;

    Ok(blocks.into_iter().map(|block| block.block).collect())
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
/// The block, or [`Error::NotFound`] if no block has the given header ID.
pub async fn get_block(
    handle: &Handle,
    header_id: HeaderId,
) -> Result<Block<UnvalidatedTx>, Error> {
    StorageApi::<UnvalidatedTx>::from_overwatch_handle(handle)
        .await?
        .try_get_block(&header_id)
        .await?
        .ok_or_else(|| Error::NotFound("Block not found".into()))
}

/// Fetch a single transaction by its hash.
///
/// # Returns
///
/// The transaction, or [`Error::NotFound`] if none has the given hash.
pub async fn get_transaction(handle: &Handle, tx_hash: TxHash) -> Result<UnvalidatedTx, Error> {
    StorageApi::<UnvalidatedTx>::from_overwatch_handle(handle)
        .await?
        .try_get_transactions(vec![tx_hash])
        .await?
        .try_next()
        .await?
        .ok_or_else(|| Error::NotFound("Transaction not found".into()))
}

/// The events of one block, or [`Error::NotFound`] if no block has the given
/// header ID.
pub async fn get_block_events(handle: &Handle, header_id: HeaderId) -> Result<Events, Error> {
    chain_api(handle)
        .await
        .get_block_events(header_id)
        .await?
        .ok_or_else(|| Error::NotFound("Block not found".into()))
}

const HEADERS_LIMIT: usize = 512;

pub async fn get_headers(
    handle: &Handle,
    from_descendant: Option<HeaderId>,
    to_ancestor: Option<HeaderId>,
) -> Result<Vec<HeaderId>, Error> {
    let chain_api = chain_api(handle).await;
    let stream = chain_api.get_headers(from_descendant, to_ancestor).await?;
    Ok(stream.take(HEADERS_LIMIT).try_collect().await?)
}

#[derive(Debug, thiserror::Error)]
pub enum BlockSlotRangeError {
    InvalidRange { slot_from: Slot, slot_to: Slot },
    SlotToExceedsLibSlot { slot_to: Slot, lib_slot: Slot },
    SlotToExceedsTipSlot { slot_to: Slot, tip_slot: Slot },
}

impl Display for BlockSlotRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRange { slot_from, slot_to } => {
                write!(
                    f,
                    "slot_to must be greater than or equal to slot_from: slot_from={slot_from:?}, \
                    slot_to={slot_to:?}"
                )
            }
            Self::SlotToExceedsLibSlot { slot_to, lib_slot } => {
                write!(
                    f,
                    "slot_to must be <= lib_slot when immutable_only=true: slot_to={slot_to:?}, \
                    lib_slot={lib_slot:?}"
                )
            }
            Self::SlotToExceedsTipSlot { slot_to, tip_slot } => {
                write!(
                    f,
                    "slot_to must be <= tip_slot: slot_to={slot_to:?}, tip_slot={tip_slot:?}"
                )
            }
        }
    }
}
