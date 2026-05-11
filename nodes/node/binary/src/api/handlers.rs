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
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_leader_service::api::ChainLeaderServiceData;
use lb_chain_service::{ConsensusMsg, Slot};
use lb_core::{
    block::Block,
    header::HeaderId,
    mantle::{
        Op, SignedMantleTx, Transaction, TxHash, gas::MainnetGasConstants, ops::channel::ChannelId,
        tx_builder::MantleTxBuilder,
    },
};
use lb_http_api_common::{
    bodies::{
        channel::{ChannelDepositRequestBody, ChannelDepositResponseBody},
        wallet::{
            balance::WalletBalanceResponseBody,
            transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
        },
    },
    paths,
};
use lb_libp2p::libp2p::bytes::Bytes;
use lb_network_service::backends::libp2p::Libp2p as Libp2pNetworkBackend;
use lb_sdp_service::{
    mempool::SdpMempoolAdapter, state::SdpStateStorage, wallet::SdpWalletAdapter,
};
use lb_storage_service::{
    StorageService, api::chain::StorageChainApi, backends::rocksdb::RocksBackend,
};
use lb_tx_service::{
    TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
};
use lb_wallet_service::api::{WalletApi, WalletServiceData};
use overwatch::{
    overwatch::handle::OverwatchHandle,
    services::{AsServiceId, ServiceData},
};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt as _;

use crate::api::{
    errors::{BlocksStreamHandlerError, BlocksStreamWindowError},
    openapi::schema,
    queries::{BlockRangeQuery, BlocksStreamQuery, BlocksStreamRequest},
    responses::{self, overwatch::get_relay_or_500},
    serializers::{
        blocks::{ApiBlock, ApiProcessedBlockEvent},
        transactions::ApiSignedTransactionRef,
    },
};

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
        Some(boundary_slot + 1)
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
    let slot_to = request.slot_to.map_or(max_slot_to, Slot::new);
    if slot_to > max_slot_to {
        let anchor = if request.immutable_only {
            "lib_slot"
        } else {
            "tip_slot"
        };
        return Err(BlocksStreamWindowError::SlotToAboveAnchor {
            anchor,
            slot_to: slot_to.into_inner(),
            max_slot_to: max_slot_to.into_inner(),
        });
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
) -> Result<Vec<ApiProcessedBlockEvent>, DynError>
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx>> + TryInto<Block<SignedMantleTx>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
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
        .map(ApiProcessedBlockEvent::from)
        .collect())
}

struct BlocksStreamState<RuntimeServiceId> {
    buffered: std::vec::IntoIter<ApiProcessedBlockEvent>,
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
    first_chunk: Vec<ApiProcessedBlockEvent>,
    slot_from: Slot,
    slot_to: Slot,
    descending: bool,
    next_cursor: Option<Slot>,
    remaining: usize,
    chunk_size: usize,
    immutable_only: bool,
) -> impl futures::Stream<Item = Result<ApiProcessedBlockEvent, DynError>>
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx>> + TryInto<Block<SignedMantleTx>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
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

#[macro_export]
macro_rules! make_request_and_return_response {
    ($cond:expr) => {{
        match $cond.await {
            ::std::result::Result::Ok(val) => ::axum::response::IntoResponse::into_response((
                ::axum::http::StatusCode::OK,
                ::axum::Json(val),
            )),
            ::std::result::Result::Err(e) => ::axum::response::IntoResponse::into_response((
                ::axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                e.to_string(),
            )),
        }
    }};
}

#[utoipa::path(
    get,
    path = paths::MANTLE_METRICS,
    responses(
        (status = 200, description = "Get the mempool metrics of the cl service", body = inline(schema::MempoolMetrics)),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn mantle_metrics<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx,
            Key = <SignedMantleTx as Transaction>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
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
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn mantle_status<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(items): Json<Vec<<SignedMantleTx as Transaction>::Hash>>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx,
            Key = <SignedMantleTx as Transaction>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Send
        + Sync
        + Display
        + 'static
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
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
        (status = 500, description = "Internal server error", body = String),
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
    path = paths::CRYPTARCHIA_HEADERS,
    responses(
        (status = 200, description = "Query header ids", body = Vec<HeaderId>),
        (status = 500, description = "Internal server error", body = String),
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
        (status = 500, description = "Internal server error", body = StreamBody),
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
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::NETWORK_INFO,
    responses(
        (status = 200, description = "Query the network information", body = lb_network_service::backends::libp2p::Libp2pInfo),
        (status = 500, description = "Internal server error", body = String),
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
        + AsServiceId<
            lb_network_service::NetworkService<
                lb_network_service::backends::libp2p::Libp2p,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(libp2p::libp2p_info::<RuntimeServiceId>(&handle))
}

#[utoipa::path(
    get,
    path = paths::BLEND_NETWORK_INFO,
    responses(
        (status = 200, description = "Query the blend network information", body = Option<lb_blend_service::message::NetworkInfo<PeerId>>),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn blend_info<BlendService, BroadcastSettings, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    BlendService: ServiceData<Message = lb_blend_service::message::ServiceMessage<BroadcastSettings, PeerId>>
        + 'static,
    BroadcastSettings: Send + 'static,
    RuntimeServiceId: Debug + Sync + Display + 'static + AsServiceId<BlendService>,
{
    make_request_and_return_response!(blend::blend_info::<
        BlendService,
        BroadcastSettings,
        RuntimeServiceId,
    >(&handle))
}

#[utoipa::path(
    post,
    path = paths::MEMPOOL_ADD_TX,
    responses(
        (status = 200, description = "Add transaction to the mempool"),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn add_tx<StorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(tx): Json<SignedMantleTx>,
) -> Response
where
    StorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx,
            Key = <SignedMantleTx as Transaction>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
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
            SignedMantleTx,
            <SignedMantleTx as Transaction>::Hash,
            RuntimeServiceId,
        >,
        StorageAdapter,
        SignedMantleTx,
        <SignedMantleTx as Transaction>::Hash,
        RuntimeServiceId,
    >(&handle, tx, Transaction::hash))
}

#[utoipa::path(
    get,
    path = paths::CHANNEL,
    responses(
        (status = 200, description = "Channel state"),
        (status = 500, description = "Internal server error", body = String),
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
    make_request_and_return_response!(mantle::channel::<RuntimeServiceId>(&handle, id))
}

#[utoipa::path(
    post,
    path = paths::CHANNEL_DEPOSIT,
    responses(
        (status = 200, description = "Submit a channel deposit"),
        (status = 500, description = "Internal server error", body = String),
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
            Item = SignedMantleTx,
            Key = <SignedMantleTx as Transaction>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    StorageAdapter::Error: Debug,
    RuntimeServiceId: Debug
        + Display
        + Send
        + Sync
        + 'static
        + AsServiceId<WalletService>
        + AsServiceId<
            TxMempoolService<
                MempoolNetworkAdapter<
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
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

        let tx_context = wallet.get_tx_context(None).await?;
        let tx_builder = MantleTxBuilder::new(tx_context).push_op(Op::ChannelDeposit(req.deposit));
        let lb_wallet_service::TipResponse {
            tip,
            response: funded_tx_builder,
        } = wallet
            .fund_tx(
                None,
                tx_builder,
                req.change_public_key,
                req.funding_public_keys,
            )
            .await?;

        let tx_fee = funded_tx_builder.gas_cost::<MainnetGasConstants>()?;
        if tx_fee > req.max_tx_fee {
            return Err(overwatch::DynError::from(format!(
                "tx_fee({tx_fee}) exceeds max_tx_fee({})",
                req.max_tx_fee
            )));
        }

        let signed_tx = wallet.sign_tx(Some(tip), funded_tx_builder).await?.response;
        let tx_hash = signed_tx.hash();

        mempool::add_tx::<
            Libp2pNetworkBackend,
            MempoolNetworkAdapter<
                SignedMantleTx,
                <SignedMantleTx as Transaction>::Hash,
                RuntimeServiceId,
            >,
            StorageAdapter,
            SignedMantleTx,
            <SignedMantleTx as Transaction>::Hash,
            RuntimeServiceId,
        >(&handle, signed_tx, Transaction::hash)
        .await?;

        Ok(ChannelDepositResponseBody { hash: tx_hash })
    })
}

#[utoipa::path(
    post,
    path = paths::SDP_POST_DECLARATION,
    responses(
        (status = 200, description = "Post declaration to SDP service", body = lb_core::sdp::DeclarationId),
        (status = 500, description = "Internal server error", body = String),
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
    StateStorage: SdpStateStorage,
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
        (status = 500, description = "Internal server error", body = String),
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
    StateStorage: SdpStateStorage,
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
        (status = 500, description = "Internal server error", body = String),
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
    StateStorage: SdpStateStorage,
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
        (status = 500, description = "Internal server error", body = String),
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
    StateStorage: SdpStateStorage,
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
    post,
    path = paths::LEADER_CLAIM,
    responses(
        (status = 200, description = "Leader claim transaction submitted", body = lb_api_service::http::consensus::leader::LeaderClaimResponseBody),
        (status = 500, description = "Internal server error", body = String),
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
        (status = 500, description = "Internal server error", body = String),
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
        TryFrom<Block<SignedMantleTx>> + TryInto<Block<SignedMantleTx>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let api_blocks =
        mantle::get_immutable_blocks(&handle, query.slot_from, query.slot_to).map(|blocks| {
            let api_blocks = blocks?.into_iter().map(ApiBlock::from).collect::<Vec<_>>();
            Ok::<Vec<ApiBlock>, DynError>(api_blocks)
        });
    make_request_and_return_response!(api_blocks)
}

#[utoipa::path(
    post,
    path = paths::BLOCKS_DETAIL,
    responses(
        (status = 200, description = "Block found"),
        (status = 404, description = "Block not found"),
        (status = 500, description = "Internal server error", body = String),
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
    let relay = match get_relay_or_500(&handle).await {
        Ok(relay) => relay,
        Err(error_response) => return error_response,
    };
    let block = HttpStorageAdapter::get_block::<SignedMantleTx>(relay, id).await;
    match block {
        Ok(Some(block)) => {
            let api_block = ApiBlock::from(block);
            (StatusCode::OK, Json(api_block)).into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND,).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    responses(
        (status = 200, description = "Stream of processed blocks with chain state"),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn blocks_stream<StorageBackend, ConsensusService, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    StorageBackend: lb_storage_service::backends::StorageBackend + Send + Sync + 'static,
    StorageBackend::Block: Serialize,
    <StorageBackend as StorageChainApi>::Block:
        TryFrom<Block<SignedMantleTx>> + TryInto<Block<SignedMantleTx>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
    ConsensusService: ServiceData<Message = ConsensusMsg<SignedMantleTx>> + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<ConsensusService>
        + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>,
{
    let stream = mantle::get_new_blocks_stream::<_, _, ConsensusService, _>(&handle)
        .await
        .map(|stream| stream.map(ApiProcessedBlockEvent::from));
    match stream {
        Ok(stream) => responses::ndjson::from_stream(stream),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    params(BlocksStreamQuery),
    responses(
        (status = 200, description = "Stream of processed blocks with chain state in slot order. \
            When immutable_only=true and slot_to is omitted, the stream anchors at LIB slot by \
            default."),
        (status = 400, description = "Invalid request parameters", body = String),
        (status = 500, description = "Internal server error", body = String),
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
        TryFrom<Block<SignedMantleTx>> + TryInto<Block<SignedMantleTx>>,
    <StorageBackend as StorageChainApi>::Tx: From<Bytes> + AsRef<[u8]>,
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
        let empty = futures::stream::empty::<ApiProcessedBlockEvent>();
        return Ok(responses::ndjson::from_stream(empty));
    }

    let consumed = first_chunk.len();
    let remaining = request.blocks_limit.get().saturating_sub(consumed);
    let boundary_slot = first_chunk
        .last()
        .map(|event| event.block.header().slot())
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
    post,
    path = paths::TRANSACTION,
    responses(
        (status = 200, description = "Transaction found"),
        (status = 404, description = "Transaction not found"),
        (status = 500, description = "Internal server error", body = String),
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
    let relay = match get_relay_or_500(&handle).await {
        Ok(relay) => relay,
        Err(error_response) => return error_response,
    };
    let Ok(transactions) = HttpStorageAdapter::get_transactions::<SignedMantleTx>(relay, id).await
    else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response();
    };
    match transactions.as_slice() {
        [] => (StatusCode::NOT_FOUND,).into_response(),
        [transaction] => {
            let api_transaction = ApiSignedTransactionRef::from(transaction);
            (StatusCode::OK, Json(api_transaction)).into_response()
        }
        _ => {
            let error_body = serde_json::json!({
                "error": "Multiple transactions found",
                "len": transactions.len()
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(error_body)).into_response()
        }
    }
}

pub mod wallet {
    use lb_http_api_common::bodies::wallet::sign::{
        WalletSignTxEd25519RequestBody, WalletSignTxEd25519ResponseBody, WalletSignTxZkRequestBody,
        WalletSignTxZkResponseBody,
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
        (status = 500, description = "Internal server error", body = String),
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
            let wallet_relay = match get_relay_or_500::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error_response) => return error_response,
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
            Ok(lb_wallet_service::TipResponse { response: None, .. }) => (
                StatusCode::NOT_FOUND,
                "The requested address could not be found in the wallet",
            )
                .into_response(),
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        }
    }

    #[utoipa::path(
    post,
    path = paths::wallet::TRANSACTIONS_TRANSFER_FUNDS,
    responses(
        (status = 200, description = "Make transfer"),
        (status = 500, description = "Internal server error", body = String),
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
                Item = SignedMantleTx,
                Key = <SignedMantleTx as Transaction>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Send
            + Sync
            + Display
            + 'static
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
                        StorageAdapter,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    RuntimeServiceId,
                >,
            >,
    {
        let wallet_api = {
            let wallet_relay = match get_relay_or_500::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error_response) => return error_response,
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
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
                        RuntimeServiceId,
                    >,
                    StorageAdapter,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >(&handle, transaction.clone(), Transaction::hash)
                .await
                {
                    return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                }

                WalletTransferFundsResponseBody::from(transaction).into_response()
            }
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        }
    }

    #[utoipa::path(
        post,
        path = paths::wallet::SIGN_TX_ED25519,
        responses(
            (status = 200, description = "Signed transaction"),
            (status = 500, description = "Internal server error", body = String),
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
                Item = SignedMantleTx,
                Key = <SignedMantleTx as Transaction>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Display
            + Send
            + Sync
            + 'static
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
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
            (status = 500, description = "Internal server error", body = String),
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
                Item = SignedMantleTx,
                Key = <SignedMantleTx as Transaction>::Hash,
            > + Send
            + Sync
            + Clone
            + 'static,
        StorageAdapter::Error: Debug,
        RuntimeServiceId: Debug
            + Display
            + Send
            + Sync
            + 'static
            + AsServiceId<WalletService>
            + AsServiceId<
                TxMempoolService<
                    MempoolNetworkAdapter<
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
                        RuntimeServiceId,
                    >,
                    Mempool<
                        HeaderId,
                        SignedMantleTx,
                        <SignedMantleTx as Transaction>::Hash,
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
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use lb_chain_service::{CryptarchiaInfo, Slot};
    use lb_core::header::HeaderId;

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
        }
    }

    fn small_chain() -> CryptarchiaInfo {
        CryptarchiaInfo {
            lib: HeaderId::from([2; 32]),
            slot: Slot::new(100),
            lib_slot: Slot::new(0),
            height: 1,
            tip: HeaderId::from([3; 32]),
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
    fn rejects_slot_to_above_tip() {
        let err = resolve_blocks_stream_window(
            &request(None, Some(TIP_SLOT + 1), true, DEFAULT_LIMIT, false),
            &chain_info(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            BlocksStreamWindowError::SlotToAboveAnchor { .. }
        ));
    }

    #[test]
    fn rejects_slot_to_above_lib_when_immutable_only() {
        let err = resolve_blocks_stream_window(
            &request(None, Some(LIB_SLOT + 1), true, DEFAULT_LIMIT, true),
            &chain_info(),
        )
        .unwrap_err();

        assert!(matches!(
            err,
            BlocksStreamWindowError::SlotToAboveAnchor { .. }
        ));
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
