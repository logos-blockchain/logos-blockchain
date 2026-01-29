use std::fmt::{Debug, Display};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use lb_api_service::http::{
    consensus::{self, Cryptarchia},
    libp2p, mantle, mempool,
    storage::StorageAdapter,
};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction},
};
use lb_http_api_common::{
    bodies::wallet::{
        balance::WalletBalanceResponseBody,
        transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
    },
    paths,
};
use lb_network_service::backends::libp2p::Libp2p as Libp2pNetworkBackend;
use lb_sdp_service::{mempool::SdpMempoolAdapter, wallet::SdpWalletAdapter};
use lb_storage_service::{StorageService, backends::rocksdb::RocksBackend};
use lb_tx_service::{
    TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
};
use lb_wallet_service::api::{WalletApi, WalletServiceData};
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use serde::Deserialize;
#[cfg(feature = "block-explorer")]
use {
    crate::api::{queries::BlockRangeQuery, serializers::blocks::ApiBlock},
    futures::FutureExt as _,
    lb_api_service::http::DynError,
    lb_chain_service::ConsensusMsg,
    lb_core::block::Block,
    lb_libp2p::libp2p::bytes::Bytes,
    lb_storage_service::api::chain::StorageChainApi,
    overwatch::services::ServiceData,
    serde::Serialize,
    tokio_stream::StreamExt as _,
};

use crate::api::{responses, responses::overwatch::get_relay_or_500};

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
        (status = 200, description = "Get the mempool metrics of the cl service", body = MempoolMetrics),
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
    post,
    path = paths::STORAGE_BLOCK,
    responses(
        (status = 200, description = "Get the block by block id", body = HeaderId),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn block<HttpStorageAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(id): Json<HeaderId>,
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
    make_request_and_return_response!(HttpStorageAdapter::get_block::<SignedMantleTx>(relay, id))
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
    post,
    path = paths::SDP_POST_DECLARATION,
    responses(
        (status = 200, description = "Post declaration to SDP service", body = lb_core::sdp::DeclarationId),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn post_declaration<MempoolAdapter, WalletAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration): Json<lb_core::sdp::DeclarationMessage>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<lb_sdp_service::SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_declaration_handler::<
        MempoolAdapter,
        WalletAdapter,
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
pub async fn post_activity<MempoolAdapter, WalletAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(metadata): Json<lb_core::sdp::ActivityMetadata>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<lb_sdp_service::SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_activity_handler::<
        MempoolAdapter,
        WalletAdapter,
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
pub async fn post_withdrawal<MempoolAdapter, WalletAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration_id): Json<lb_core::sdp::DeclarationId>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    WalletAdapter: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<lb_sdp_service::SdpService<MempoolAdapter, WalletAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(lb_api_service::http::sdp::post_withdrawal_handler::<
        MempoolAdapter,
        WalletAdapter,
        RuntimeServiceId,
    >(handle, declaration_id))
}

#[cfg(feature = "block-explorer")]
#[utoipa::path(
    get,
    path = paths::BLOCKS,
    params(BlockRangeQuery),
    responses(
        (status = 200, description = "Get blocks"),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn blocks<StorageBackend, RuntimeServiceId>(
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
    let api_blocks = mantle::get_blocks(&handle, query.slot_from, query.slot_to).map(|blocks| {
        let api_blocks = blocks?.into_iter().map(ApiBlock::from).collect::<Vec<_>>();
        Ok::<Vec<ApiBlock>, DynError>(api_blocks)
    });
    make_request_and_return_response!(api_blocks)
}

#[cfg(feature = "block-explorer")]
#[utoipa::path(
    get,
    path = paths::BLOCKS_STREAM,
    responses(
        (status = 200, description = "Get blocks"),
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
        .map(|stream| stream.map(ApiBlock::from));
    match stream {
        Ok(stream) => responses::ndjson::from_stream(stream),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

pub mod wallet {
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
                balance,
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
    pub async fn post_transactions_transfer_funds<WalletService, RuntimeServiceId>(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(body): Json<WalletTransferFundsRequestBody>,
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
            }) => WalletTransferFundsResponseBody::from(transaction).into_response(),
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        }
    }
}
