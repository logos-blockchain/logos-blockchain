use std::num::NonZeroUsize;

use futures::Stream;
use lb_api_common::{
    DEFAULT_BLOCKS_STREAM_CHUNK_SIZE, DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM, MAX_BLOCKS_STREAM_BLOCKS,
    queries::{BlockFilter, BlockSortOrder, BlocksStreamQuery},
};
use lb_chain_service::{CryptarchiaInfo, Slot};
use lb_log_targets::node;
use tracing::debug;
use validator::Validate as _;

use super::{
    Error, Handle,
    blocks::{BlockWithChainState, get_blocks_in_slot_range_with_snapshot},
    cryptarchia,
};
use crate::UnvalidatedTx;

const TARGET: &str = node::api::ROOT;

/// The blocks in the slot window `query` asks for, newest first unless it says
/// otherwise, fetched from storage one chunk at a time as the stream is
/// consumed.
pub async fn stream(
    handle: &Handle,
    query: BlocksStreamQuery,
) -> Result<impl Stream<Item = Result<BlockWithChainState<UnvalidatedTx>, Error>> + use<>, Error> {
    let request = BlocksStreamRequest::try_from(query)?;

    let chain_info = cryptarchia::info(handle).await?.cryptarchia_info;

    let ResolvedBlocksStreamWindow { slot_from, slot_to } =
        resolve_blocks_stream_window(&request, &chain_info)?;

    let first_chunk_limit = NonZeroUsize::new(
        request
            .server_batch_size
            .get()
            .min(request.blocks_limit.get()),
    )
    .expect("chunk size min blocks limit should be non-zero");

    let first_chunk = get_blocks_in_slot_range_with_snapshot(
        handle,
        slot_from,
        slot_to,
        request.descending,
        first_chunk_limit,
        request.immutable_only,
        &chain_info,
    )
    .await?;

    let remaining = request.blocks_limit.get().saturating_sub(first_chunk.len());
    let next_cursor = first_chunk.last().and_then(|event| {
        next_blocks_stream_cursor(
            request.descending,
            slot_from,
            slot_to,
            event.block.header().slot(),
        )
    });

    Ok(build_blocks_stream(
        handle.clone(),
        chain_info,
        first_chunk,
        slot_from,
        slot_to,
        request.descending,
        next_cursor,
        remaining,
        request.server_batch_size.get(),
        request.immutable_only,
    ))
}

/// This is a processed `BlocksStreamQuery` with all defaults applied and
/// validated, ready to be used for fetching blocks.
struct BlocksStreamRequest {
    /// An optional lower bound slot.
    slot_from: Option<u64>,
    /// An optional upper bound slot (inclusive).
    slot_to: Option<u64>,
    /// Sort direction, either descending or the opposite.
    descending: bool,
    /// Maximum number of blocks to return.
    blocks_limit: NonZeroUsize,
    /// Server chunk size hint for streamed delivery.
    server_batch_size: NonZeroUsize,
    /// When true, include only immutable blocks.
    immutable_only: bool,
}

impl TryFrom<BlocksStreamQuery> for BlocksStreamRequest {
    type Error = BlocksStreamRequestError;

    // Parse and validate the query parameters for the blocks stream endpoint,
    // applying defaults where necessary.
    fn try_from(query: BlocksStreamQuery) -> Result<Self, Self::Error> {
        query.validate()?;

        let blocks_limit = query.blocks_limit.unwrap_or_else(|| {
            if query.slot_from.is_some() && query.slot_to.is_some() {
                NonZeroUsize::new(MAX_BLOCKS_STREAM_BLOCKS).unwrap()
            } else {
                NonZeroUsize::new(DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM).unwrap()
            }
        });

        let server_batch_size = query
            .server_batch_size
            .unwrap_or(NonZeroUsize::new(DEFAULT_BLOCKS_STREAM_CHUNK_SIZE).unwrap());

        if let (Some(slot_from), Some(slot_to)) = (query.slot_from, query.slot_to)
            && slot_from > slot_to
        {
            return Err(BlocksStreamRequestError::InvalidSlotRange { slot_from, slot_to });
        }

        Ok(Self {
            slot_from: query.slot_from,
            slot_to: query.slot_to,
            descending: query.order.unwrap_or(BlockSortOrder::Descending)
                == BlockSortOrder::Descending,
            blocks_limit,
            server_batch_size,
            immutable_only: query
                .block_filter
                .unwrap_or(BlockFilter::MutableAndImmutable)
                == BlockFilter::ImmutableOnly,
        })
    }
}

#[derive(Debug, thiserror::Error)]
enum BlocksStreamRequestError {
    #[error("invalid query: {0}")]
    Validation(#[from] validator::ValidationErrors),
    #[error("'slot_from' must be <= 'slot_to', got slot_from={slot_from}, slot_to={slot_to}")]
    InvalidSlotRange { slot_from: u64, slot_to: u64 },
}

/// Errors that can occur during resolving the blocks stream window from the
/// request and chain info.
#[derive(Debug, thiserror::Error)]
enum BlocksStreamWindowError {
    #[error("'slot_to' must be <= {anchor}, got slot_to={slot_to}, {anchor}={max_slot_to}")]
    SlotToAboveAnchor {
        anchor: &'static str,
        slot_to: u64,
        max_slot_to: u64,
    },
    #[error("'slot_from' must be <= 'slot_to', got slot_from={slot_from}, slot_to={slot_to}")]
    SlotFromAboveSlotTo { slot_from: u64, slot_to: u64 },
}

impl From<BlocksStreamRequestError> for Error {
    fn from(error: BlocksStreamRequestError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<BlocksStreamWindowError> for Error {
    fn from(error: BlocksStreamWindowError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

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
    chain_info: &CryptarchiaInfo,
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
    chain_info: &CryptarchiaInfo,
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

struct BlocksStreamState {
    buffered: std::vec::IntoIter<BlockWithChainState<UnvalidatedTx>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
    chain_info: CryptarchiaInfo,
    handle: Handle,
}

#[expect(clippy::too_many_arguments, reason = "Need all args")]
fn build_blocks_stream(
    handle: Handle,
    chain_info: CryptarchiaInfo,
    first_chunk: Vec<BlockWithChainState<UnvalidatedTx>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
) -> impl Stream<Item = Result<BlockWithChainState<UnvalidatedTx>, Error>> {
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

            let fetched_blocks = get_blocks_in_slot_range_with_snapshot(
                &state.handle,
                chunk_from,
                chunk_to,
                state.descending,
                request_limit,
                state.immutable_only,
                &state.chain_info,
            )
            .await;
            let next_chunk = match fetched_blocks {
                Ok(next_chunk) => next_chunk,
                Err(error) => {
                    // Terminal error: avoid re-emitting the same error forever.
                    state.remaining = 0;
                    state.next_cursor = None;
                    return Some((Err(error), state));
                }
            };

            if next_chunk.is_empty() {
                return None;
            }

            let boundary_slot = next_chunk
                .last()
                .map(|event| event.block.header().slot())
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

#[cfg(test)]
mod tests {

    use lb_core::header::HeaderId;

    use super::*;

    #[test]
    fn blocks_stream_request_defaults_to_recent_blocks_from_tip() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query without explicit range should parse");

        assert_eq!(
            request.blocks_limit,
            NonZeroUsize::new(DEFAULT_NUMBER_OF_BLOCKS_TO_STREAM).unwrap()
        );
        assert_eq!(request.slot_from, None);
        assert_eq!(request.slot_to, None);
        assert!(request.descending);
        assert_eq!(
            request.server_batch_size,
            NonZeroUsize::new(DEFAULT_BLOCKS_STREAM_CHUNK_SIZE).unwrap()
        );
        assert!(!request.immutable_only);
    }

    #[test]
    fn blocks_stream_request_accepts_blocks_limit_only() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: None,
            slot_to: None,
            order: None,
            blocks_limit: NonZeroUsize::new(7),
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query with explicit blocks_limit should parse");

        assert_eq!(request.blocks_limit, NonZeroUsize::new(7).unwrap());
        assert_eq!(request.slot_to, None);
    }

    #[test]
    fn blocks_stream_request_accepts_slot_range() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Descending),
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        })
        .expect("query with explicit slot range should parse");

        assert_eq!(
            request.blocks_limit,
            NonZeroUsize::new(MAX_BLOCKS_STREAM_BLOCKS).unwrap()
        );
        assert_eq!(request.slot_from, Some(10));
        assert_eq!(request.slot_to, Some(20));
        assert!(request.descending);
    }

    #[test]
    fn blocks_stream_request_maps_typed_order_and_filter() {
        let request = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(10),
            slot_to: Some(20),
            order: Some(BlockSortOrder::Ascending),
            blocks_limit: NonZeroUsize::new(7),
            server_batch_size: NonZeroUsize::new(3),
            block_filter: Some(BlockFilter::ImmutableOnly),
        })
        .expect("typed query should parse");

        assert!(!request.descending);
        assert_eq!(request.blocks_limit, NonZeroUsize::new(7).unwrap());
        assert_eq!(request.server_batch_size, NonZeroUsize::new(3).unwrap());
        assert!(request.immutable_only);
    }

    #[test]
    fn blocks_stream_query_rejects_zero_limit_at_deserialization() {
        let result = serde_urlencoded::from_str::<BlocksStreamQuery>("blocks_limit=0");

        assert!(
            result.is_err(),
            "zero blocks_limit should fail to deserialize"
        );
    }

    #[test]
    fn blocks_stream_query_rejects_zero_batch_size_at_deserialization() {
        let result = serde_urlencoded::from_str::<BlocksStreamQuery>("server_batch_size=0");

        assert!(
            result.is_err(),
            "zero server_batch_size should fail to deserialize"
        );
    }

    #[test]
    fn blocks_stream_request_rejects_invalid_slot_range() {
        let result = BlocksStreamRequest::try_from(BlocksStreamQuery {
            slot_from: Some(9),
            slot_to: Some(7),
            order: None,
            blocks_limit: None,
            server_batch_size: None,
            block_filter: None,
        });

        match result.err() {
            Some(BlocksStreamRequestError::InvalidSlotRange { .. }) => {}
            _ => panic!("Expected validation error for 'slot_from' > 'slot_to'"),
        }
    }

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
