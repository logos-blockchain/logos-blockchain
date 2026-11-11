use std::{fmt::Debug, num::NonZeroUsize};

use axum::{
    Json,
    extract::{Path, Query, State},
    response::{IntoResponse as _, Response},
};
use lb_api_common::{paths, queries::BlocksStreamQuery};
use lb_chain_service::Slot;
use lb_core::{
    header::HeaderId,
    mantle::{TxHash, ledger::verification_mode::StandardMode, transactions::states::Unverified},
};
use lb_log_targets::node;
use serde::Deserialize;
use tokio_stream::StreamExt as _;
use tracing::debug;

use crate::api::{
    calls::{self, DynError, Handle, blocks, cryptarchia},
    errors::{BlocksStreamHandlerError, BlocksStreamWindowError, ErrorBody},
    queries::{BlockRangeQuery, BlocksStreamRequest},
    responses::{self},
    serializers::{
        blocks::{ApiBlock, ApiBlockOwned, ApiProcessedBlockEventOwned},
        transactions::ApiSignedTransaction,
    },
};

const TARGET: &str = node::api::ROOT;

#[derive(Debug)]
struct ResolvedBlocksStreamWindow {
    slot_from: Slot,
    slot_to: Slot,
}

fn next_blocks_stream_cursor(
    descending: bool,
    slot_from: Slot,
    slot_to: Slot,
    boundary_slot: Slot,
) -> Option<Slot> {
    if descending {
        if boundary_slot <= slot_from {
            None
        } else {
            Some(boundary_slot.saturating_sub(Slot::new(1)))
        }
    } else if boundary_slot >= slot_to {
        None
    } else {
        Some(boundary_slot.strict_add(1.into()))
    }
}

fn resolve_blocks_stream_window(
    request: &BlocksStreamRequest,
    chain_info: &lb_chain_service::CryptarchiaInfo,
) -> Result<ResolvedBlocksStreamWindow, BlocksStreamWindowError> {
    let max_slot_to = if request.immutable_only {
        chain_info.lib_slot
    } else {
        chain_info.slot
    };
    let mut slot_to = request.slot_to.map_or(max_slot_to, Slot::new);
    if slot_to > max_slot_to {
        slot_to = max_slot_to;
        let anchor = if request.immutable_only {
            "lib_slot"
        } else {
            "tip_slot"
        };
        debug!(
            target: TARGET,
            "{}: clamping to {}",
            BlocksStreamWindowError::SlotToAboveAnchor {
                anchor,
                slot_to: slot_to.into_inner(),
                max_slot_to: max_slot_to.into_inner(),
            }.to_string(),
            max_slot_to.into_inner()
        );
    }

    let slot_from = request.slot_from.map_or_else(
        || default_slot_from_for_blocks_stream(request, chain_info, slot_to),
        Slot::new,
    );
    if slot_from > slot_to {
        return Err(BlocksStreamWindowError::SlotFromAboveSlotTo {
            slot_from: slot_from.into_inner(),
            slot_to: slot_to.into_inner(),
        });
    }

    Ok(ResolvedBlocksStreamWindow { slot_from, slot_to })
}

fn default_slot_from_for_blocks_stream(
    request: &BlocksStreamRequest,
    chain_info: &lb_chain_service::CryptarchiaInfo,
    slot_to: Slot,
) -> Slot {
    const ASCENDING_ESTIMATED_WINDOW_NOMINATOR: u64 = 2;
    const ASCENDING_ESTIMATED_WINDOW_DENOMINATOR: u64 = 3;

    if request.descending {
        return Slot::new(0);
    }

    let average_slots_per_block = chain_info
        .slot
        .into_inner()
        .div_ceil(chain_info.height.max(1));

    // For ascending streams without an explicit slot_from, estimate a narrow
    // window ending near slot_to. This prioritizes ending close to slot_to over
    // guaranteeing blocks_limit returned blocks.
    let estimated_slot_span = (request.blocks_limit.get() as u64)
        .saturating_mul(average_slots_per_block.max(1))
        .saturating_mul(ASCENDING_ESTIMATED_WINDOW_NOMINATOR)
        / ASCENDING_ESTIMATED_WINDOW_DENOMINATOR;

    slot_to.saturating_sub(Slot::new(estimated_slot_span))
}

async fn fetch_blocks_stream_chunk(
    handle: &Handle,
    chain_info: &lb_chain_service::CryptarchiaInfo,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    blocks_limit: NonZeroUsize,
    immutable_only: bool,
) -> Result<Vec<ApiProcessedBlockEventOwned<Unverified, StandardMode>>, calls::Error> {
    let chunk = blocks::get_blocks_in_slot_range_with_snapshot(
        handle,
        slot_from,
        slot_to,
        descending,
        blocks_limit,
        immutable_only,
        chain_info,
    )
    .await?;

    Ok(chunk
        .into_iter()
        .map(ApiProcessedBlockEventOwned::from)
        .collect())
}

struct BlocksStreamState {
    buffered: std::vec::IntoIter<ApiProcessedBlockEventOwned<Unverified, StandardMode>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
    chain_info: lb_chain_service::CryptarchiaInfo,
    handle: Handle,
}

#[expect(clippy::too_many_arguments, reason = "Need all args")]
fn build_blocks_stream(
    handle: Handle,
    chain_info: lb_chain_service::CryptarchiaInfo,
    first_chunk: Vec<ApiProcessedBlockEventOwned<Unverified, StandardMode>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
) -> impl futures::Stream<Item = Result<ApiProcessedBlockEventOwned<Unverified, StandardMode>, DynError>>
{
    let state = BlocksStreamState {
        buffered: first_chunk.into_iter(),
        slot_from,
        slot_to,
        descending,
        next_cursor,
        remaining,
        chunk_size,
        immutable_only,
        chain_info,
        handle,
    };

    futures::stream::unfold(state, async |mut state| {
        loop {
            if let Some(item) = state.buffered.next() {
                return Some((Ok(item), state));
            }

            let cursor = match state.next_cursor {
                Some(cursor) if state.remaining > 0 => cursor,
                _ => return None,
            };

            let request_limit = NonZeroUsize::new(state.chunk_size.min(state.remaining))
                .expect("remaining and chunk size are non-zero");
            let (chunk_from, chunk_to) = if state.descending {
                (state.slot_from, cursor)
            } else {
                (cursor, state.slot_to)
            };

            let fetched_blocks = fetch_blocks_stream_chunk(
                &state.handle,
                &state.chain_info,
                chunk_from,
                chunk_to,
                state.descending,
                request_limit,
                state.immutable_only,
            )
            .await;
            let next_chunk = match fetched_blocks {
                Ok(next_chunk) => next_chunk,
                Err(error) => {
                    // Terminal error: avoid re-emitting the same error forever.
                    state.remaining = 0;
                    state.next_cursor = None;
                    return Some((Err(error.into()), state));
                }
            };

            if next_chunk.is_empty() {
                return None;
            }

            let boundary_slot = next_chunk
                .last()
                .map(|event| event.block().header().slot())
                .expect("non-empty chunk has a last element");

            state.next_cursor = next_blocks_stream_cursor(
                state.descending,
                state.slot_from,
                state.slot_to,
                boundary_slot,
            );

            state.remaining = state.remaining.saturating_sub(next_chunk.len());
            state.buffered = next_chunk.into_iter();
        }
    })
}

#[derive(Deserialize)]
pub struct CryptarchiaInfoQuery {
    from: Option<HeaderId>,
    to: Option<HeaderId>,
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_HEADERS,
    responses(
        (status = 200, description = "Query header ids", body = Vec<HeaderId>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_headers(
    State(handle): State<Handle>,
    Query(query): Query<CryptarchiaInfoQuery>,
) -> Response {
    let CryptarchiaInfoQuery { from, to } = query;
    make_request_and_return_response!(blocks::get_headers(&handle, from, to))
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_LIB_STREAM,
    responses(
        (status = 200, description = "Request a stream for lib blocks"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_lib_stream(State(handle): State<Handle>) -> Response {
    let stream = blocks::lib_block_stream(&handle).await;
    match stream {
        Ok(stream) => responses::ndjson::from_stream_result(stream),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS,
    params(BlockRangeQuery),
    responses(
        (status = 200, description = "Get blocks"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn immutable_blocks(
    State(handle): State<Handle>,
    Query(query): Query<BlockRangeQuery>,
) -> Response {
    make_request_and_return_response!(async {
        let blocks = blocks::get_immutable_blocks(&handle, query.slot_from, query.slot_to).await?;
        Ok::<_, calls::Error>(
            blocks
                .into_iter()
                .map(ApiBlockOwned::from)
                .collect::<Vec<_>>(),
        )
    })
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_DETAIL,
    responses(
        (status = 200, description = "Block found"),
        (status = 404, description = "Block not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn block(State(handle): State<Handle>, Path(id): Path<HeaderId>) -> Response {
    match blocks::get_block(&handle, id).await {
        Ok(block) => Json(ApiBlock::from(&block)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCK_EVENTS,
    responses(
        (status = 200, description = "Block events", body = Object),
        (status = 404, description = "Block not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn block_events(State(handle): State<Handle>, Path(id): Path<HeaderId>) -> Response {
    make_request_and_return_response!(blocks::get_block_events(&handle, id))
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    responses(
        (status = 200, description = "Stream of processed blocks with chain state"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blocks_stream(State(handle): State<Handle>) -> Response {
    let stream = blocks::get_new_blocks_stream(&handle)
        .await
        .map(|stream| stream.map(ApiProcessedBlockEventOwned::from));
    match stream {
        Ok(stream) => responses::ndjson::from_stream(stream),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_RANGE_STREAM,
    params(BlocksStreamQuery),
    responses(
        (status = 200, description = "Stream of processed blocks with chain state in slot order. \
            When immutable_only=true and slot_to is omitted, the stream anchors at LIB slot by \
            default."),
        (status = 400, description = "Invalid request parameters", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blocks_range_stream(
    State(handle): State<Handle>,
    Query(query): Query<BlocksStreamQuery>,
) -> Result<Response, BlocksStreamHandlerError> {
    let request = BlocksStreamRequest::try_from(query)?;

    let chain_info = cryptarchia::info(&handle).await?;

    let resolved_window = resolve_blocks_stream_window(&request, &chain_info.cryptarchia_info)?;
    let slot_from = resolved_window.slot_from;
    let slot_to = resolved_window.slot_to;

    let first_chunk_limit = NonZeroUsize::new(
        request
            .server_batch_size
            .get()
            .min(request.blocks_limit.get()),
    )
    .expect("chunk size min blocks limit should be non-zero");

    let first_chunk = fetch_blocks_stream_chunk(
        &handle,
        &chain_info.cryptarchia_info,
        slot_from,
        slot_to,
        request.descending,
        first_chunk_limit,
        request.immutable_only,
    )
    .await?;

    if first_chunk.is_empty() {
        let empty =
            futures::stream::empty::<ApiProcessedBlockEventOwned<Unverified, StandardMode>>();
        return Ok(responses::ndjson::from_stream(empty));
    }

    let consumed = first_chunk.len();
    let remaining = request.blocks_limit.get().saturating_sub(consumed);
    let boundary_slot = first_chunk
        .last()
        .map(|event| event.block().header().slot())
        .expect("non-empty chunk has a last element");

    let next_cursor =
        next_blocks_stream_cursor(request.descending, slot_from, slot_to, boundary_slot);

    let stream = build_blocks_stream(
        handle,
        chain_info.cryptarchia_info,
        first_chunk,
        slot_from,
        slot_to,
        request.descending,
        next_cursor,
        remaining,
        request.server_batch_size.get(),
        request.immutable_only,
    );

    Ok(responses::ndjson::from_stream_result(stream))
}

#[utoipa::path(
    get,
    path = paths::TRANSACTION,
    responses(
        (status = 200, description = "Transaction found"),
        (status = 404, description = "Transaction not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn transaction(State(handle): State<Handle>, Path(id): Path<TxHash>) -> Response {
    match blocks::get_transaction(&handle, id).await {
        Ok(transaction) => Json(ApiSignedTransaction::from(&transaction)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use lb_chain_service::CryptarchiaInfo;

    use super::*;

    const TIP_SLOT: u64 = 100_000;

    const LIB_SLOT: u64 = 80_000;

    const HEIGHT: u64 = 500;

    const DEFAULT_LIMIT: usize = 100;

    const DEFAULT_BATCH_SIZE: usize = 100;

    fn chain_info() -> CryptarchiaInfo {
        CryptarchiaInfo {
            lib: HeaderId::from([2; 32]),
            slot: Slot::new(TIP_SLOT),
            lib_slot: Slot::new(LIB_SLOT),
            height: HEIGHT,
            tip: HeaderId::from([3; 32]),
            state: lb_chain_service::State::Online,
        }
    }

    fn small_chain() -> CryptarchiaInfo {
        CryptarchiaInfo {
            lib: HeaderId::from([2; 32]),
            slot: Slot::new(100),
            lib_slot: Slot::new(0),
            height: 1,
            tip: HeaderId::from([3; 32]),
            state: lb_chain_service::State::Online,
        }
    }

    fn request(
        slot_from: Option<u64>,
        slot_to: Option<u64>,
        descending: bool,
        blocks_limit: usize,
        immutable_only: bool,
    ) -> BlocksStreamRequest {
        BlocksStreamRequest {
            slot_from,
            slot_to,
            descending,
            blocks_limit: NonZeroUsize::new(blocks_limit).unwrap(),
            server_batch_size: NonZeroUsize::new(DEFAULT_BATCH_SIZE).unwrap(),
            immutable_only,
        }
    }

    #[test]
    fn default_slot_to_is_tip() {
        let window = resolve_blocks_stream_window(
            &request(None, None, true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_to, Slot::new(TIP_SLOT));
    }

    #[test]
    fn immutable_only_default_slot_to_is_lib() {
        let window = resolve_blocks_stream_window(
            &request(None, None, true, DEFAULT_LIMIT, true),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_to, Slot::new(LIB_SLOT));
    }

    #[test]
    fn clamps_slot_to_above_tip() {
        let window = resolve_blocks_stream_window(
            &request(None, Some(TIP_SLOT + 1), true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_to, Slot::new(TIP_SLOT));
    }

    #[test]
    fn clamps_slot_to_above_lib_when_immutable_only() {
        let window = resolve_blocks_stream_window(
            &request(None, Some(LIB_SLOT + 1), true, DEFAULT_LIMIT, true),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_to, Slot::new(LIB_SLOT));
    }

    #[test]
    fn accepts_slot_to_equal_to_tip() {
        assert!(
            resolve_blocks_stream_window(
                &request(None, Some(TIP_SLOT), true, DEFAULT_LIMIT, false),
                &chain_info()
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_slot_to_equal_to_lib_when_immutable_only() {
        assert!(
            resolve_blocks_stream_window(
                &request(None, Some(LIB_SLOT), true, DEFAULT_LIMIT, true),
                &chain_info()
            )
            .is_ok()
        );
    }

    #[test]
    fn descending_without_slot_from_defaults_to_zero() {
        let window = resolve_blocks_stream_window(
            &request(None, None, true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_from, Slot::new(0));
    }

    #[test]
    fn explicit_slot_from_is_used_for_descending() {
        let window = resolve_blocks_stream_window(
            &request(Some(2_000), Some(9_000), true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_from, Slot::new(2_000));
        assert_eq!(window.slot_to, Slot::new(9_000));
    }

    #[test]
    fn explicit_slot_from_is_used_for_ascending() {
        let window = resolve_blocks_stream_window(
            &request(Some(3_000), Some(9_000), false, 50, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_from, Slot::new(3_000));
        assert_eq!(window.slot_to, Slot::new(9_000));
    }

    #[test]
    fn ascending_without_slot_from_estimates_lower_bound_from_slot_to() {
        let average_slots_per_block = TIP_SLOT.div_ceil(500); // 200
        // The explicit `2 / 3` locks the behaviour in.
        let estimated_span = DEFAULT_LIMIT as u64 * average_slots_per_block * 2 / 3; // 13_333
        let slot_from = TIP_SLOT - estimated_span; // 86_667
        let window = resolve_blocks_stream_window(
            &request(None, Some(TIP_SLOT), false, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap();

        assert_eq!(window.slot_from, Slot::new(slot_from));
        assert_eq!(window.slot_to, Slot::new(TIP_SLOT));
    }

    #[test]
    fn ascending_without_slot_from_estimate_saturates_to_zero() {
        let request = request(None, Some(50), false, 1_000, false);
        let window = resolve_blocks_stream_window(&request, &small_chain()).unwrap();

        assert_eq!(window.slot_from, Slot::new(0));
    }

    #[test]
    fn rejects_explicit_slot_from_above_slot_to() {
        let err = resolve_blocks_stream_window(
            &request(Some(9_000), Some(8_000), true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            BlocksStreamWindowError::SlotFromAboveSlotTo { .. }
        ));
    }
}
