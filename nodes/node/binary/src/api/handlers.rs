use std::{
    fmt::{Debug, Display},
    num::NonZeroUsize,
};

use ::libp2p::PeerId;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use futures::FutureExt as _;
use lb_api_service::http::{
    DynError, blend,
    consensus::{self, Cryptarchia},
    libp2p, mantle, mempool,
    storage::StorageAdapter,
};
use lb_blend_service::message::ProxyServiceMessage;
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_leader_service::api::ChainLeaderServiceData;
use lb_chain_service::{ConsensusMsg, Slot, api::CryptarchiaServiceApi};
use lb_core::{
    block::Block,
    events::Events,
    header::HeaderId,
    mantle::{
        Op, OpProof, SignedMantleTx, TxHash,
        channel::ChannelState,
        ops::channel::ChannelId,
        traits::Hashable,
        transactions::{
            MantleTxBuilder,
            states::{Preverified, Unverified},
        },
    },
};
use lb_http_api_common::{
    TimeInfo,
    bodies::{
        blend::JoinBlendRequestBody,
        channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
        mantle::GasPricesResponseBody,
        wallet::{
            balance::WalletBalanceResponseBody,
            claimable_vouchers::{
                ClaimableVoucherInfoResponseBody, WalletClaimableVouchersResponseBody,
            },
            transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
        },
    },
    paths,
    queries::BlocksStreamQuery,
};
use lb_libp2p::{Multiaddr, libp2p::bytes::Bytes};
use lb_log_targets::node;
use lb_network_service::{NetworkService, backends::libp2p::Libp2p as Libp2pNetworkBackend};
use lb_sdp_service::{
    mempool::SdpMempoolAdapter, state::SdpStateStorage, wallet::SdpWalletAdapter,
};
use lb_storage_service::{
    StorageService, api::chain::StorageChainApi, backends::rocksdb::RocksBackend,
};
use lb_time_service::TimeServiceMessage;
use lb_tx_service::{
    MempoolMsg, TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
};
use lb_wallet_service::api::{WalletApi, WalletServiceData};
use overwatch::{
    overwatch::handle::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_stream::StreamExt as _;
use tracing::debug;

use crate::{
    TimeService,
    api::{
        errors::{ApiError, BlocksStreamHandlerError, BlocksStreamWindowError},
        openapi::schema,
        queries::{BlockRangeQuery, BlocksStreamRequest},
        responses::{self, overwatch::get_relay},
        serializers::{
            blocks::{ApiBlock, ApiBlockOwned, ApiProcessedBlockEventOwned},
            transactions::ApiSignedTransaction,
        },
    },
};

const TARGET: &str = node::api::ROOT;

fn validate_max_tx_fee(
    tx_fee: lb_core::mantle::gas::GasCost,
    max_tx_fee: lb_core::mantle::gas::GasCost,
) -> Result<(), DynError> {
    if tx_fee > max_tx_fee {
        return Err(overwatch::DynError::from(format!(
            "tx_fee({tx_fee}) exceeds max_tx_fee({max_tx_fee})"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DialPeerRequestBody {
    pub addr: Multiaddr,
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

async fn fetch_blocks_stream_chunk<StorageBackend, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    chain_info: &lb_chain_service::CryptarchiaInfo,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    blocks_limit: NonZeroUsize,
    immutable_only: bool,
) -> Result<Vec<ApiProcessedBlockEventOwned<Unverified>>, DynError>
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx<Unverified>>> + TryInto<Block<SignedMantleTx<Unverified>>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <StorageBackend as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let chunk = mantle::get_blocks_in_slot_range_with_snapshot::<_, _, RuntimeServiceId>(
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

struct BlocksStreamState<RuntimeServiceId> {
    buffered: std::vec::IntoIter<ApiProcessedBlockEventOwned<Unverified>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
    chain_info: lb_chain_service::CryptarchiaInfo,
    handle: OverwatchHandle<RuntimeServiceId>,
}

#[expect(clippy::too_many_arguments, reason = "Need all args")]
fn build_blocks_stream<StorageBackend, RuntimeServiceId>(
    handle: OverwatchHandle<RuntimeServiceId>,
    chain_info: lb_chain_service::CryptarchiaInfo,
    first_chunk: Vec<ApiProcessedBlockEventOwned<Unverified>>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
) -> impl futures::Stream<Item = Result<ApiProcessedBlockEventOwned<Unverified>, DynError>>
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx<Unverified>>> + TryInto<Block<SignedMantleTx<Unverified>>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <StorageBackend as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
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

            let fetched_blocks = fetch_blocks_stream_chunk::<StorageBackend, RuntimeServiceId>(
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
                    return Some((Err(error), state));
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

#[macro_export]
macro_rules! make_request_and_return_response {
    ($cond:expr) => {{ $crate::api::errors::json_response($cond.await) }};
}

#[utoipa::path(
    get,
    path = paths::MANTLE_METRICS,
    responses(
        (status = 200, description = "Get the mempool metrics of the cl service", body = inline(schema::MempoolMetrics)),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mantle_metrics<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(mantle::mantle_mempool_metrics::<
        StorageAdapter,
        RuntimeServiceId,
    >(&handle))
}

#[utoipa::path(
    post,
    path = paths::MANTLE_STATUS,
    responses(
        (status = 200, description = "Query the mempool status of the cl service", body = Vec<<T as Transaction>::Hash>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mantle_status<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(items): Json<Vec<<SignedMantleTx<Preverified> as Hashable>::Hash>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(mantle::mantle_mempool_status::<
        StorageAdapter,
        RuntimeServiceId,
    >(&handle, items))
}

#[utoipa::path(
    get,
    path = paths::NODE_VERSION,
    responses(
        (status = 200, description = "Version of the running node, e.g. `0.1.2 (abcdefaa)`", body = String),
    )
)]
#[expect(
    clippy::unused_async,
    reason = "Axum handlers are required to be async."
)]
pub async fn version() -> Response {
    Json(crate::version::node_version()).into_response()
}

#[derive(Deserialize)]
pub struct CryptarchiaInfoQuery {
    from: Option<HeaderId>,
    to: Option<HeaderId>,
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_INFO,
    responses(
        (status = 200, description = "Query consensus information", body = lb_consensus::CryptarchiaInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_info<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    make_request_and_return_response!(consensus::cryptarchia_info::<RuntimeServiceId>(&handle))
}

#[utoipa::path(
    get,
    path = paths::TIME_INFO,
    responses(
        (status = 200, description = "Query time service information", body = TimeInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn time_info<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<TimeService>,
{
    let relay = match handle.relay::<TimeService>().await {
        Ok(relay) => relay,
        Err(error) => {
            return ApiError::internal(error).into_response();
        }
    };
    let (sender, receiver) = oneshot::channel();
    if let Err((error, _)) = relay.send(TimeServiceMessage::Info { sender }).await {
        return ApiError::internal(error).into_response();
    }
    match receiver.await {
        Ok(Ok(service_info)) => {
            let api_info = TimeInfo {
                slot_duration_ms: service_info.slot_duration_ms,
                genesis_time_unix_ms: service_info.genesis_time_unix_ms,
                current_slot: u64::from(service_info.current_slot),
                current_epoch: u32::from(service_info.current_epoch),
            };
            (StatusCode::OK, Json(api_info)).into_response()
        }
        Ok(Err(error)) => ApiError::internal_message(error).into_response(),
        Err(error) => ApiError::internal(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_HEADERS,
    responses(
        (status = 200, description = "Query header ids", body = Vec<HeaderId>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_headers<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Query(query): Query<CryptarchiaInfoQuery>,
) -> Response
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    let CryptarchiaInfoQuery { from, to } = query;
    make_request_and_return_response!(consensus::cryptarchia_headers::<RuntimeServiceId>(
        &handle, from, to
    ))
}

#[utoipa::path(
    get,
    path = paths::CRYPTARCHIA_LIB_STREAM,
    responses(
        (status = 200, description = "Request a stream for lib blocks"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn cryptarchia_lib_stream<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId:
        Debug + Sync + Display + AsServiceId<BlockBroadcastService<RuntimeServiceId>> + 'static,
{
    let stream = mantle::lib_block_stream(&handle).await;
    match stream {
        Ok(stream) => responses::ndjson::from_stream_result(stream),
        Err(error) => ApiError::Internal(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::NETWORK_INFO,
    responses(
        (status = 200, description = "Query the network information", body = lb_network_service::backends::libp2p::Libp2pInfo),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn libp2p_info<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<NetworkService<Libp2pNetworkBackend, RuntimeServiceId>>,
{
    make_request_and_return_response!(libp2p::libp2p_info::<RuntimeServiceId>(&handle))
}

#[utoipa::path(
    post,
    path = paths::DIAL_PEER,
    request_body = DialPeerRequestBody,
    responses(
        (status = 200, description = "Dial a network peer", body = PeerId),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn dial_peer<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<DialPeerRequestBody>,
) -> Response
where
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<NetworkService<Libp2pNetworkBackend, RuntimeServiceId>>,
{
    make_request_and_return_response!(async move {
        let peer_id = libp2p::connect_peer::<RuntimeServiceId>(&handle, req.addr).await?;
        Ok::<PeerId, overwatch::DynError>(peer_id)
    })
}

#[utoipa::path(
    get,
    path = paths::BLEND_NETWORK_INFO,
    responses(
        (status = 200, description = "Query the blend network information", body = Option<lb_blend_service::message::NetworkInfo<PeerId>>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_info<BlendService, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    BlendService: ServiceData<
            Message = ProxyServiceMessage<lb_blend_service::message::ServiceMessage<PeerId>>,
        > + 'static,
    RuntimeServiceId: Debug + Sync + Display + 'static + AsServiceId<BlendService>,
{
    make_request_and_return_response!(blend::blend_info::<BlendService, RuntimeServiceId>(&handle))
}

#[utoipa::path(
    post,
    path = paths::BLEND_JOIN_NETWORK,
    request_body = BlendJoinNetworkRequestBody,
    responses(
        (status = 200, description = "Join the blend network", body = Option<lb_core::sdp::DeclarationId>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blend_join_network<BlendService, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<JoinBlendRequestBody>,
) -> Response
where
    BlendService: ServiceData<
            Message = ProxyServiceMessage<lb_blend_service::message::ServiceMessage<PeerId>>,
        > + 'static,
    RuntimeServiceId: Debug + Sync + Display + 'static + AsServiceId<BlendService>,
{
    make_request_and_return_response!(blend::blend_join_network::<BlendService, RuntimeServiceId>(
        &handle,
        req.locator,
        req.locked_note_id
    ))
}

#[utoipa::path(
    post,
    path = paths::MEMPOOL_ADD_TX,
    responses(
        (status = 200, description = "Add transaction to the mempool"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn add_tx<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(tx): Json<SignedMantleTx<Preverified>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(mempool::add_tx::<
        Libp2pNetworkBackend,
        MempoolNetworkAdapter<
            SignedMantleTx<Preverified>,
            <SignedMantleTx<Preverified> as Hashable>::Hash,
            RuntimeServiceId,
        >,
        StorageAdapter,
        SignedMantleTx<Preverified>,
        <SignedMantleTx<Preverified> as Hashable>::Hash,
        RuntimeServiceId,
    >(&handle, tx, Hashable::hash))
}

#[utoipa::path(
    get,
    path = paths::MEMPOOL_VIEW,
    responses(
        (status = 200, description = "Get current tip mempool transaction hashes", body = Vec<TxHash>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn mempool_view<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(
        current_tip_mempool_view::<StorageAdapter, RuntimeServiceId>(&handle)
    )
}

async fn current_tip_mempool_view<StorageAdapter, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
) -> Result<Vec<TxHash>, DynError>
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let consensus = consensus::cryptarchia_info::<RuntimeServiceId>(handle).await?;

    mempool_view_at::<StorageAdapter, RuntimeServiceId>(handle, consensus.cryptarchia_info.tip)
        .await
}

async fn mempool_view_at<StorageAdapter, RuntimeServiceId>(
    handle: &OverwatchHandle<RuntimeServiceId>,
    ancestor_hint: HeaderId,
) -> Result<Vec<TxHash>, DynError>
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    let relay = handle
        .relay::<TxMempoolService<
            MempoolNetworkAdapter<
                SignedMantleTx<Preverified>,
                <SignedMantleTx<Preverified> as Hashable>::Hash,
                RuntimeServiceId,
            >,
            Mempool<
                HeaderId,
                SignedMantleTx<Preverified>,
                <SignedMantleTx<Preverified> as Hashable>::Hash,
                StorageAdapter,
                RuntimeServiceId,
            >,
            StorageAdapter,
            RuntimeServiceId,
        >>()
        .await?;
    let (sender, receiver) = oneshot::channel();

    relay
        .send(MempoolMsg::View {
            ancestor_hint,
            reply_channel: sender,
        })
        .await
        .map_err(|(error, _)| error)?;

    let txs = receiver.await?;

    Ok(
        tokio_stream::StreamExt::map(txs, |tx: SignedMantleTx<Preverified>| tx.hash())
            .collect()
            .await,
    )
}

#[utoipa::path(
    get,
    path = paths::CHANNEL,
    responses(
        (status = 200, description = "Channel state"),
        (status = 404, description = "Channel not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn channel<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Path(id): Path<ChannelId>,
) -> Response
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    channel_response(mantle::channel_state::<RuntimeServiceId>(&handle, id).await)
}

fn channel_response(result: Result<Option<ChannelState>, DynError>) -> Response {
    match result {
        Ok(Some(channel)) => (StatusCode::OK, Json(channel)).into_response(),
        Ok(None) => ApiError::NotFound("Channel not found".into()).into_response(),
        Err(error) => ApiError::Internal(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = paths::CHANNEL_DEPOSIT,
    responses(
        (status = 200, description = "Submit a channel deposit"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn channel_deposit<WalletService, StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<ChannelDepositRequestBody>,
) -> Response
where
    WalletService: WalletServiceData,
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx<Preverified>,
            Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Clone
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
        + AsServiceId<WalletService>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
                StorageAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(async {
        let wallet = WalletApi::<WalletService, RuntimeServiceId>::new(
            handle.relay::<WalletService>().await?,
        );

        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::ChannelDeposit(req.deposit))
            .map_err(|e| overwatch::DynError::from(e.to_string()))?;
        let lb_wallet_service::TipResponse {
            tip,
            response: funded_tx_builder,
        } = wallet
            .fund_tx(
                None,
                tx_builder,
                req.change_public_key,
                req.funding_public_keys,
                0,
            )
            .await?;

        let tx_fee = funded_tx_builder.tx_fee()?;
        validate_max_tx_fee(tx_fee, req.max_tx_fee)?;

        let signed_tx = wallet.sign_tx(Some(tip), funded_tx_builder).await?.response;
        let tx_hash = signed_tx.hash();

        mempool::add_tx::<
            Libp2pNetworkBackend,
            MempoolNetworkAdapter<
                SignedMantleTx<Preverified>,
                <SignedMantleTx<Preverified> as Hashable>::Hash,
                RuntimeServiceId,
            >,
            StorageAdapter,
            SignedMantleTx<Preverified>,
            <SignedMantleTx<Preverified> as Hashable>::Hash,
            RuntimeServiceId,
        >(&handle, signed_tx, Hashable::hash)
        .await?;

        Ok::<_, overwatch::DynError>(ChannelDepositResponseBody { hash: tx_hash })
    })
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_DECLARATION,
    responses(
        (status = 200, description = "Post declaration to SDP service", body = lb_core::sdp::DeclarationId),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_declaration<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration): Json<lb_core::sdp::DeclarationMessage>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: lb_chain_service::api::CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<
            lb_sdp_service::SdpService<
                MempoolAdapter,
                WalletAdapter,
                ChainService,
                StateStorage,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_declaration_handler::<
        MempoolAdapter,
        WalletAdapter,
        ChainService,
        StateStorage,
        RuntimeServiceId,
    >(handle, declaration))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_ACTIVITY,
    responses(
        (status = 200, description = "Post activity to SDP service"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_activity<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(metadata): Json<lb_core::sdp::ActivityMetadata>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: lb_chain_service::api::CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<
            lb_sdp_service::SdpService<
                MempoolAdapter,
                WalletAdapter,
                ChainService,
                StateStorage,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_activity_handler::<
        MempoolAdapter,
        WalletAdapter,
        ChainService,
        StateStorage,
        RuntimeServiceId,
    >(handle, metadata))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_WITHDRAWAL,
    responses(
        (status = 200, description = "Post withdrawal to SDP service"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_withdrawal<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration_id): Json<lb_core::sdp::DeclarationId>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: lb_chain_service::api::CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<
            lb_sdp_service::SdpService<
                MempoolAdapter,
                WalletAdapter,
                ChainService,
                StateStorage,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_withdrawal_handler::<
        MempoolAdapter,
        WalletAdapter,
        ChainService,
        StateStorage,
        RuntimeServiceId,
    >(handle, declaration_id))
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_SET_DECLARATION_ID,
    responses(
        (status = 200, description = "Post declaration to SDP service to be set as current", body = lb_core::sdp::DeclarationId),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn post_set_declaration_id<
    MempoolAdapter,
    WalletAdapter,
    ChainService,
    StateStorage,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration): Json<Option<lb_core::sdp::DeclarationId>>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    ChainService: lb_chain_service::api::CryptarchiaServiceData + Send + Sync + 'static,
    StateStorage: SdpStateStorage<RuntimeServiceId>,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<ChainService>
        + AsServiceId<
            lb_sdp_service::SdpService<
                MempoolAdapter,
                WalletAdapter,
                ChainService,
                StateStorage,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(
        lb_api_service::http::sdp::post_set_declaration_id_handler::<
            MempoolAdapter,
            WalletAdapter,
            ChainService,
            StateStorage,
            RuntimeServiceId,
        >(handle, declaration)
    )
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_DECLARATIONS,
    responses(
        (status = 200, description = "Get current SDP declarations keyed by declaration id", body = std::collections::HashMap<lb_core::sdp::DeclarationId, lb_core::sdp::Declaration>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_sdp_declarations<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    make_request_and_return_response!(mantle::get_sdp_declarations::<RuntimeServiceId>(&handle))
}

#[utoipa::path(
    get,
    path = paths::MANTLE_SDP_SNAPSHOT,
    responses(
        (status = 200, description = "Get the SDP snapshot for the current epoch keyed by declaration id", body = std::collections::HashMap<lb_core::sdp::DeclarationId, lb_core::sdp::Declaration>),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_sdp_snapshot<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    RuntimeServiceId:
        Debug + Send + Sync + Display + 'static + AsServiceId<Cryptarchia<RuntimeServiceId>>,
{
    make_request_and_return_response!(mantle::get_sdp_snapshot::<RuntimeServiceId>(&handle))
}

#[utoipa::path(
    post,
    path = paths::LEADER_CLAIM,
    responses(
        (status = 200, description = "Leader claim transaction submitted", body = lb_api_service::http::consensus::leader::LeaderClaimResponseBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn leader_claim<ChainLeader, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    ChainLeader: ChainLeaderServiceData,
    RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<ChainLeader>,
{
    make_request_and_return_response!(consensus::leader::claim(&handle))
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
pub async fn immutable_blocks<StorageBackend, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Query(query): Query<BlockRangeQuery>,
) -> Response
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static, /* TODO: StorageChainApi */
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx<Unverified>>> + TryInto<Block<SignedMantleTx<Unverified>>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <StorageBackend as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let api_blocks =
        mantle::get_immutable_blocks(&handle, query.slot_from, query.slot_to).map(|blocks| {
            let api_blocks = blocks?
                .into_iter()
                .map(ApiBlockOwned::from)
                .collect::<Vec<_>>();
            Ok::<Vec<ApiBlockOwned<Unverified>>, DynError>(api_blocks)
        });
    make_request_and_return_response!(api_blocks)
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
pub async fn block<HttpStorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Path(id): Path<HeaderId>,
) -> Response
where
    HttpStorageAdapter: StorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId:
        AsServiceId<StorageService<RocksBackend, RuntimeServiceId>> + Debug + Sync + Display,
{
    let relay = match get_relay(&handle).await {
        Ok(relay) => relay,
        Err(error) => return error.into_response(),
    };
    let block = HttpStorageAdapter::get_block::<SignedMantleTx<Unverified>>(relay, id).await;
    match block {
        Ok(Some(block)) => {
            let api_block = ApiBlock::from(&block);
            (StatusCode::OK, Json(api_block)).into_response()
        }
        Ok(None) => ApiError::NotFoundEmpty.into_response(),
        Err(_) => ApiError::InternalServerError.into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCK_EVENTS,
    responses(
        (status = 200, description = "Block events", body = Events),
        (status = 404, description = "Block not found", body = ErrorBody),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn block_events<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Path(id): Path<HeaderId>,
) -> Response
where
    RuntimeServiceId:
        AsServiceId<Cryptarchia<RuntimeServiceId>> + Debug + Sync + Display + Send + 'static,
{
    let relay = match get_relay(&handle).await {
        Ok(relay) => relay,
        Err(error) => return error.into_response(),
    };
    let chain_api =
        CryptarchiaServiceApi::<Cryptarchia<RuntimeServiceId>, RuntimeServiceId>::new(relay);

    match chain_api.get_block_events(id).await {
        Ok(Some(events)) => (StatusCode::OK, Json(events)).into_response(),
        Ok(None) => ApiError::NotFound("Block not found".into()).into_response(),
        Err(_) => ApiError::InternalServerError.into_response(),
    }
}

#[derive(Deserialize)]
pub struct GasPricesQuery {
    tip: Option<HeaderId>,
}

#[utoipa::path(
    get,
    path = paths::MANTLE_GAS_PRICES,
    responses(
        (status = 200, description = "Get the gas prices from the ledger state at the tip"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn get_gas_prices<RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Query(query): Query<GasPricesQuery>,
) -> Response
where
    RuntimeServiceId:
        AsServiceId<Cryptarchia<RuntimeServiceId>> + Debug + Sync + Display + Send + 'static,
{
    let relay = match get_relay(&handle).await {
        Ok(relay) => relay,
        Err(error) => return error.into_response(),
    };
    let chain_api =
        CryptarchiaServiceApi::<Cryptarchia<RuntimeServiceId>, RuntimeServiceId>::new(relay);

    let block_id = match query.tip {
        Some(tip) => tip,
        None => match consensus::cryptarchia_info::<RuntimeServiceId>(&handle).await {
            Ok(info) => info.cryptarchia_info.tip,
            Err(error) => {
                return ApiError::Internal(error).into_response();
            }
        },
    };

    match chain_api.get_ledger_state(block_id).await {
        Ok(Some(ledger_state)) => {
            let gas_prices = ledger_state.get_gas_prices();
            Json(GasPricesResponseBody {
                tip: block_id,
                execution_base_gas_price: gas_prices.execution_base_gas_price,
                storage_gas_price: gas_prices.storage_gas_price,
            })
            .into_response()
        }
        Ok(None) => ApiError::NotFound("Ledger state not found for block".into()).into_response(),
        Err(error) => ApiError::internal(error).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    responses(
        (status = 200, description = "Stream of processed blocks with chain state"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
)]
pub async fn blocks_stream<StorageBackend, ConsensusService, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx<Preverified>>> + TryInto<Block<SignedMantleTx<Preverified>>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <StorageBackend as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    ConsensusService: ServiceData<Message = ConsensusMsg<SignedMantleTx<Preverified>>> + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<ConsensusService>
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let stream = mantle::get_new_blocks_stream::<_, _, ConsensusService, _>(&handle)
        .await
        .map(|stream| stream.map(ApiProcessedBlockEventOwned::from));
    match stream {
        Ok(stream) => responses::ndjson::from_stream(stream),
        Err(error) => ApiError::Internal(error).into_response(),
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
pub async fn blocks_range_stream<StorageBackend, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Query(query): Query<BlocksStreamQuery>,
) -> Result<Response, BlocksStreamHandlerError>
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx<Unverified>>> + TryInto<Block<SignedMantleTx<Unverified>>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    <StorageBackend as StorageChainApi>::Events: TryFrom<Events> + TryInto<Events>,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let request = BlocksStreamRequest::try_from(query)?;

    let chain_info = consensus::cryptarchia_info::<RuntimeServiceId>(&handle).await?;

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

    let first_chunk = fetch_blocks_stream_chunk::<StorageBackend, RuntimeServiceId>(
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
        let empty = futures::stream::empty::<ApiProcessedBlockEventOwned<Unverified>>();
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

    let stream = build_blocks_stream::<StorageBackend, RuntimeServiceId>(
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
pub async fn transaction<HttpStorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Path(id): Path<TxHash>,
) -> Response
where
    HttpStorageAdapter: StorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId:
        AsServiceId<StorageService<RocksBackend, RuntimeServiceId>> + Debug + Sync + Display,
{
    let relay = match get_relay(&handle).await {
        Ok(relay) => relay,
        Err(error) => return error.into_response(),
    };
    let Ok(transactions) =
        HttpStorageAdapter::get_transactions::<SignedMantleTx<Unverified>>(relay, id).await
    else {
        return ApiError::InternalServerError.into_response();
    };
    match transactions.as_slice() {
        [] => ApiError::NotFoundEmpty.into_response(),
        [transaction] => {
            let api_transaction = ApiSignedTransaction::from(transaction);
            (StatusCode::OK, Json(api_transaction)).into_response()
        }
        _ => ApiError::internal_message(format!(
            "Multiple transactions found ({})",
            transactions.len()
        ))
        .into_response(),
    }
}

pub mod wallet {
    use lb_http_api_common::bodies::wallet::{
        fund::{WalletFundRequestBody, WalletFundResponseBody},
        sign::{
            WalletSignTxEd25519RequestBody, WalletSignTxEd25519ResponseBody,
            WalletSignTxZkRequestBody, WalletSignTxZkResponseBody,
        },
    };
    use lb_key_management_system_service::keys::ZkPublicKey;

    use super::*;

    #[derive(Deserialize)]
    pub struct TipQuery {
        tip: Option<HeaderId>,
    }

    #[utoipa::path(
    get,
    path = paths::wallet::BALANCE,
    responses(
        (status = 200, description = "Get wallet balance"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
    )]
    pub async fn get_balance<WalletService, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Path(address): Path<ZkPublicKey>,
        Query(query): Query<TipQuery>,
    ) -> Response
    where
        WalletService: WalletServiceData + 'static,
        RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<WalletService>,
    {
        let wallet_api = {
            let wallet_relay = match get_relay::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error) => return error.into_response(),
            };
            WalletApi::<WalletService, RuntimeServiceId>::new(wallet_relay)
        };

        let balance = wallet_api.get_balance(query.tip, address).await;
        match balance {
            Ok(lb_wallet_service::TipResponse {
                tip,
                response: Some(balance),
            }) => WalletBalanceResponseBody {
                tip,
                balance: balance.balance,
                notes: balance.notes,
                address,
            }
            .into_response(),
            Ok(lb_wallet_service::TipResponse { response: None, .. }) => {
                ApiError::NotFound("The requested address could not be found in the wallet".into())
                    .into_response()
            }
            Err(error) => ApiError::internal(error).into_response(),
        }
    }

    #[utoipa::path(
    get,
    path = paths::LEADER_CLAIM_VOUCHERS,
    responses(
        (status = 200, description = "Get claimable wallet vouchers"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
    )]
    pub async fn get_claimable_vouchers<WalletService, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Query(query): Query<TipQuery>,
    ) -> Response
    where
        WalletService: WalletServiceData + 'static,
        RuntimeServiceId: Debug + Send + Sync + Display + 'static + AsServiceId<WalletService>,
    {
        let wallet_relay = match get_relay::<WalletService, _>(&handle).await {
            Ok(relay) => relay,
            Err(error) => return error.into_response(),
        };
        let wallet_api = WalletApi::<WalletService, RuntimeServiceId>::new(wallet_relay);

        match wallet_api.get_claimable_vouchers(query.tip).await {
            Ok(lb_wallet_service::TipResponse { tip, response }) => {
                let vouchers = response
                    .into_iter()
                    .map(|voucher| ClaimableVoucherInfoResponseBody {
                        commitment: voucher.commitment,
                        nullifier: voucher.nullifier,
                    })
                    .collect();

                WalletClaimableVouchersResponseBody { tip, vouchers }.into_response()
            }
            Err(error) => ApiError::internal(error).into_response(),
        }
    }

    #[utoipa::path(
    post,
    path = paths::wallet::TRANSACTIONS_TRANSFER_FUNDS,
    responses(
        (status = 200, description = "Make transfer"),
        (status = 500, description = "Internal server error", body = ErrorBody),
    )
    )]
    pub async fn post_transactions_transfer_funds<WalletService, StorageAdapter, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(body): Json<WalletTransferFundsRequestBody>,
    ) -> Response
    where
        WalletService: WalletServiceData + 'static,
        StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Item = SignedMantleTx<Preverified>,
                Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Clone
            + Send
            + Sync
            + Display
            + 'static
            + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        StorageAdapter,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
            >,
    {
        let wallet_api = {
            let wallet_relay = match get_relay::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error) => return error.into_response(),
            };
            WalletApi::<WalletService, RuntimeServiceId>::new(wallet_relay)
        };

        let transfer_funds = wallet_api
            .transfer_funds(
                body.tip,
                body.change_public_key,
                body.funding_public_keys,
                body.recipient_public_key,
                body.amount,
            )
            .await;

        match transfer_funds {
            Ok(lb_wallet_service::TipResponse {
                response: transaction,
                ..
            }) => {
                // Submit to mempool
                if let Err(e) = mempool::add_tx::<
                    Libp2pNetworkBackend,
                    MempoolNetworkAdapter<
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    SignedMantleTx<Preverified>,
                    <SignedMantleTx<Preverified> as Hashable>::Hash,
                    RuntimeServiceId,
                >(&handle, transaction.clone(), Hashable::hash)
                .await
                {
                    return ApiError::Internal(e).into_response();
                }

                WalletTransferFundsResponseBody::from(transaction).into_response()
            }
            Err(error) => ApiError::internal(error).into_response(),
        }
    }

    #[utoipa::path(
        post,
        path = paths::wallet::SIGN_TX_ED25519,
        responses(
            (status = 200, description = "Signed transaction"),
            (status = 500, description = "Internal server error", body = ErrorBody),
        )
    )]
    pub async fn sign_tx_ed25519<WalletService, StorageAdapter, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(req): Json<WalletSignTxEd25519RequestBody>,
    ) -> Response
    where
        WalletService: WalletServiceData,
        StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Item = SignedMantleTx<Preverified>,
                Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Clone
            + Display
            + Send
            + Sync
            + 'static
            + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        StorageAdapter,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
            >,
    {
        make_request_and_return_response!(async {
            let wallet = WalletApi::<WalletService, RuntimeServiceId>::new(
                handle.relay::<WalletService>().await?,
            );

            let sig = wallet.sign_tx_with_ed25519(req.tx_hash, req.pk).await?;
            Ok::<_, DynError>(WalletSignTxEd25519ResponseBody { sig })
        })
    }

    #[utoipa::path(
        post,
        path = paths::wallet::SIGN_TX_ZK,
        responses(
            (status = 200, description = "Signed transaction"),
            (status = 500, description = "Internal server error", body = ErrorBody),
        )
    )]
    pub async fn sign_tx_zk<WalletService, StorageAdapter, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(req): Json<WalletSignTxZkRequestBody>,
    ) -> Response
    where
        WalletService: WalletServiceData,
        StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Item = SignedMantleTx<Preverified>,
                Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Clone
            + Display
            + Send
            + Sync
            + 'static
            + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        StorageAdapter,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
            >,
    {
        make_request_and_return_response!(async {
            let wallet = WalletApi::<WalletService, RuntimeServiceId>::new(
                handle.relay::<WalletService>().await?,
            );

            let sig = wallet.sign_tx_with_zk(req.tx_hash, req.pks).await?;
            Ok::<_, DynError>(WalletSignTxZkResponseBody { sig })
        })
    }

    #[utoipa::path(
        post,
        path = paths::wallet::FUND,
        responses(
            (status = 200, description = "Funded transaction with fee transfer proof"),
            (status = 500, description = "Internal server error", body = ErrorBody),
        )
    )]
    pub async fn fund<WalletService, StorageAdapter, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(req): Json<WalletFundRequestBody>,
    ) -> Response
    where
        WalletService: WalletServiceData,
        StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Item = SignedMantleTx<Preverified>,
                Key = <SignedMantleTx<Preverified> as Hashable>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Clone
            + Display
            + Send
            + Sync
            + 'static
            + AsServiceId<StorageService<StorageAdapter::Backend, RuntimeServiceId>>
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx<Preverified>,
                        <SignedMantleTx<Preverified> as Hashable>::Hash,
                        StorageAdapter,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
            >,
    {
        make_request_and_return_response!(async {
            let wallet = WalletApi::<WalletService, RuntimeServiceId>::new(
                handle.relay::<WalletService>().await?,
            );

            let lb_wallet_service::TipResponse {
                tip,
                response: funded_tx_builder,
            } = wallet
                .fund_tx(
                    req.tip,
                    req.tx_builder,
                    req.change_public_key,
                    req.funding_public_keys,
                    req.priority_fee_percent,
                )
                .await?;

            let tx_fee = funded_tx_builder.tx_fee()?;
            validate_max_tx_fee(tx_fee, req.max_tx_fee)?;

            // Owners of the funding inputs, in input order — the ledger
            // verifies the transfer proof against this exact list.
            let funding_note_pks: Vec<ZkPublicKey> = funded_tx_builder
                .ledger_inputs()
                .iter()
                .map(|utxo| utxo.note.pk)
                .collect();

            let funded_tx = funded_tx_builder.build()?;
            let transfer_proof = if funding_note_pks.is_empty() {
                None
            } else {
                let tx_hash = funded_tx.hash();
                Some(OpProof::ZkSig(
                    wallet.sign_tx_with_zk(tx_hash, funding_note_pks).await?,
                ))
            };

            Ok::<_, DynError>(WalletFundResponseBody {
                tip,
                funded_tx,
                transfer_proof,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc};

    use axum::{body, http::StatusCode};
    use lb_api_service::http::DynError;
    use lb_chain_service::{CryptarchiaInfo, Slot};
    use lb_core::{
        header::HeaderId,
        mantle::{
            channel::{ChannelState, SlotTimeframe, SlotTimeout},
            gas::GasCost,
            ops::channel::{Ed25519PublicKey, MsgId, config::Keys},
        },
    };

    use super::{channel_response, validate_max_tx_fee};
    use crate::api::{
        errors::BlocksStreamWindowError, handlers::resolve_blocks_stream_window,
        queries::BlocksStreamRequest,
    };

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

    fn channel_state() -> ChannelState {
        let accredited_keys: Keys =
            [Ed25519PublicKey::from_bytes(&[0; 32]).expect("test public key should be valid")]
                .into();

        ChannelState {
            accredited_keys: Arc::new(accredited_keys),
            configuration_threshold: 1,
            tip_message: MsgId::root(),
            tip_slot: Slot::default(),
            tip_sequencer: 0,
            tip_sequencer_starting_slot: Slot::default(),
            posting_timeframe: SlotTimeframe::from(0),
            posting_timeout: SlotTimeout::from(0),
            transfer_threshold: 1,
        }
    }

    #[test]
    fn channel_response_returns_ok_for_existing_channel() {
        let response = channel_response(Ok(Some(channel_state())));

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn channel_response_returns_typed_not_found_for_missing_channel() {
        let response = channel_response(Ok(None));

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response body should be valid JSON");
        assert_eq!(
            body,
            serde_json::json!({
                "code": StatusCode::NOT_FOUND.as_u16(),
                "message": "Channel not found",
            })
        );
    }

    #[test]
    fn channel_response_returns_internal_error_for_backend_failure() {
        let response = channel_response(Err(DynError::from("channel backend failed".to_owned())));

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn max_tx_fee_rejects_a_percentage_funded_final_fee() {
        let mandatory_fee: u64 = 794;
        let priority_fee_percent: u64 = 12;
        let priority_fee_amount = (mandatory_fee * priority_fee_percent).div_ceil(100);
        let funded_tx_fee = GasCost::new(mandatory_fee + priority_fee_amount);

        let error = validate_max_tx_fee(funded_tx_fee, GasCost::new(mandatory_fee + 95))
            .expect_err("the percentage reserve should be included in the hard cap");
        assert!(error.to_string().contains("exceeds max_tx_fee"));
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
