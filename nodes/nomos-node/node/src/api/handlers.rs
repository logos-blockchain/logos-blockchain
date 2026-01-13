use std::{
    error::Error,
    fmt::{Debug, Display},
    hash::Hash,
};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use broadcast_service::BlockBroadcastService;
use nomos_api::http::{
    consensus::{self, Cryptarchia},
    da::{self, BalancerMessageFactory, DaVerifier, MonitorMessageFactory},
    libp2p, mantle, mempool,
    storage::StorageAdapter,
};
use nomos_core::{
    da::{BlobId, DaVerifier as CoreDaVerifier, blob::Share},
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction},
    sdp::SessionNumber,
};
use nomos_da_messages::http::da::{
    DASharesCommitmentsRequest, DaSamplingRequest, GetSharesRequest,
};
use nomos_da_network_service::{
    NetworkService, api::ApiAdapter as ApiAdapterTrait, backends::NetworkBackend,
    sdp::SdpAdapter as SdpAdapterTrait,
};
use nomos_da_sampling::{DaSamplingService, backend::DaSamplingServiceBackend};
use nomos_da_verifier::{backend::VerifierBackend, mempool::DaMempoolAdapter};
use nomos_http_api_common::{
    bodies::wallet::{
        balance::WalletBalanceResponseBody,
        transfer_funds::{WalletTransferFundsRequestBody, WalletTransferFundsResponseBody},
    },
    paths,
};
use nomos_libp2p::PeerId;
use nomos_network::backends::libp2p::Libp2p as Libp2pNetworkBackend;
use nomos_sdp::adapters::mempool::SdpMempoolAdapter;
use nomos_storage::{StorageService, api::da::DaConverter, backends::rocksdb::RocksBackend};
use nomos_wallet::api::{WalletApi, WalletServiceData};
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use subnetworks_assignations::MembershipHandler;
use tracing::error;
use tx_service::{
    TxMempoolService, backend::Mempool,
    network::adapters::libp2p::Libp2pAdapter as MempoolNetworkAdapter,
};
#[cfg(feature = "block-explorer")]
use {
    crate::api::{queries::BlockRangeQuery, serializers::blocks::ApiBlock},
    chain_service::ConsensusMsg,
    futures::FutureExt as _,
    nomos_api::http::DynError,
    nomos_core::block::Block,
    nomos_libp2p::libp2p::bytes::Bytes,
    nomos_storage::api::chain::StorageChainApi,
    overwatch::services::ServiceData,
    tokio_stream::StreamExt as _,
};

use crate::api::{backend::DaStorageBackend, responses, responses::overwatch::get_relay_or_500};

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
    StorageAdapter: tx_service::storage::MempoolStorageAdapter<
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
    StorageAdapter: tx_service::storage::MempoolStorageAdapter<
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
        (status = 200, description = "Query consensus information", body = nomos_consensus::CryptarchiaInfo),
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
    post,
    path = paths::DA_ADD_SHARE,
    responses(
        (status = 200, description = "Share to be published received"),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn add_share<S, N, VB, StorageConverter, VerifierMempoolAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(share): Json<S>,
) -> Response
where
    S: Share + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <S as Share>::BlobId: Clone + Send + Sync + 'static,
    <S as Share>::ShareIndex: Clone + Hash + Eq + Send + Sync + 'static,
    <S as Share>::LightShare: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <S as Share>::SharesCommitments: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    N: nomos_da_verifier::network::NetworkAdapter<RuntimeServiceId>,
    N::Settings: Clone,
    VB: VerifierBackend + CoreDaVerifier<DaShare = S>,
    <VB as VerifierBackend>::Settings: Clone,
    <VB as CoreDaVerifier>::Error: Error,
    StorageConverter:
        DaConverter<DaStorageBackend, Share = S, Tx = SignedMantleTx> + Send + Sync + 'static,
    VerifierMempoolAdapter: DaMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            DaVerifier<S, N, VB, StorageConverter, VerifierMempoolAdapter, RuntimeServiceId>,
        >,
{
    make_request_and_return_response!(da::add_share::<
        S,
        N,
        VB,
        StorageConverter,
        VerifierMempoolAdapter,
        RuntimeServiceId,
    >(&handle, share))
}

#[utoipa::path(
    post,
    path = paths::DA_BLOCK_PEER,
    responses(
        (status = 200, description = "Block a peer", body = bool),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn block_peer<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(peer_id): Json<PeerId>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::block_peer::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >(&handle, peer_id))
}

#[utoipa::path(
    post,
    path = paths::DA_UNBLOCK_PEER,
    responses(
        (status = 200, description = "Unblock a peer", body = bool),
        (status = 500, description = "Internal server error", body = String),
    )
)]

pub async fn unblock_peer<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(peer_id): Json<PeerId>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::unblock_peer::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >(&handle, peer_id))
}

#[utoipa::path(
    get,
    path = paths::DA_BLACKLISTED_PEERS,
    responses(
        (status = 200, description = "Get the blacklisted peers", body = Vec<PeerId>),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn blacklisted_peers<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::blacklisted_peers::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >(&handle))
}

#[utoipa::path(
    get,
    path = paths::NETWORK_INFO,
    responses(
        (status = 200, description = "Query the network information", body = nomos_network::backends::libp2p::Libp2pInfo),
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
            nomos_network::NetworkService<
                nomos_network::backends::libp2p::Libp2p,
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

#[derive(Serialize, Deserialize)]
pub struct GetCommitmentsRequest<DaBlobId> {
    pub blob_id: DaBlobId,
    pub session: SessionNumber,
}

#[utoipa::path(
    get,
    path = paths::DA_GET_SHARES_COMMITMENTS,
    responses(
        (status = 200, description = "Request the commitments for an specific `BlobId` that the node stores locally or otherwise requests from the subnetwork peers", body = DASharesCommitmentsRequest<DaShare>),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn da_get_commitments<
    DaBlobId,
    SamplingBackend,
    SamplingNetwork,
    SamplingStorage,
    SamplingMempoolAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<GetCommitmentsRequest<DaBlobId>>,
) -> Response
where
    DaBlobId: Serialize + for<'de> Deserialize<'de> + Send + 'static,
    SamplingBackend: DaSamplingServiceBackend<BlobId = DaBlobId>,
    SamplingNetwork: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
    SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
    SamplingMempoolAdapter: nomos_da_sampling::mempool::DaMempoolAdapter,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetwork,
                SamplingStorage,
                SamplingMempoolAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::get_commitments::<
        SamplingBackend,
        SamplingNetwork,
        SamplingMempoolAdapter,
        SamplingStorage,
        RuntimeServiceId,
    >(&handle, req.blob_id, req.session))
}

#[utoipa::path(
    get,
    path = paths::DA_GET_STORAGE_SHARES_COMMITMENTS,
    responses(
        (status = 200, description = "Request the commitments for an specific `BlobId` that the node stores locally", body = DASharesCommitmentsRequest<DaShare>),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn da_get_storage_commitments<
    DaStorageConverter,
    HttpStorageAdapter,
    DaShare,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(req): Json<DASharesCommitmentsRequest<DaShare>>,
) -> Response
where
    DaShare: Share,
    <DaShare as Share>::BlobId: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <DaShare as Share>::SharesCommitments: Serialize + DeserializeOwned + Send + Sync + 'static,
    DaStorageConverter: DaConverter<DaStorageBackend, Share = DaShare> + Send + Sync + 'static,
    HttpStorageAdapter: StorageAdapter<RuntimeServiceId>,
    RuntimeServiceId: AsServiceId<StorageService<DaStorageBackend, RuntimeServiceId>>
        + Debug
        + Sync
        + Display
        + 'static,
{
    let relay = match get_relay_or_500(&handle).await {
        Ok(relay) => relay,
        Err(error_response) => return error_response,
    };
    make_request_and_return_response!(HttpStorageAdapter::get_shared_commitments::<
        DaStorageConverter,
        DaShare,
    >(relay, req.blob_id))
}

#[utoipa::path(
    get,
    path = paths::DA_GET_LIGHT_SHARE,
    responses(
        (status = 200, description = "Get blob by blob id", body = DaSamplingRequest<DaShare>),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn da_get_light_share<DaStorageConverter, HttpStorageAdapter, DaShare, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(request): Json<DaSamplingRequest<DaShare>>,
) -> Response
where
    DaShare: Share + Clone + Send + Sync + 'static,
    <DaShare as Share>::BlobId: Clone + DeserializeOwned + Send + Sync + 'static,
    <DaShare as Share>::ShareIndex: Clone + DeserializeOwned + Send + Sync + 'static,
    DaShare::LightShare: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    DaStorageConverter: DaConverter<RocksBackend, Share = DaShare> + Send + Sync + 'static,
    HttpStorageAdapter: StorageAdapter<RuntimeServiceId>,
    RuntimeServiceId: AsServiceId<StorageService<RocksBackend, RuntimeServiceId>>
        + Debug
        + Sync
        + Display
        + 'static,
{
    let relay = match get_relay_or_500(&handle).await {
        Ok(relay) => relay,
        Err(error_response) => return error_response,
    };
    make_request_and_return_response!(HttpStorageAdapter::get_light_share::<
        DaStorageConverter,
        DaShare,
    >(relay, request.blob_id, request.share_idx))
}

#[utoipa::path(
    get,
    path = paths::DA_GET_LIGHT_SHARE,
    responses(
        (status = 200, description = "Request shares for a blob", body = GetSharesRequest<DaBlob>),
        (status = 500, description = "Internal server error", body = StreamBody),
    )
)]
pub async fn da_get_shares<DaStorageConverter, HttpStorageAdapter, DaShare, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(request): Json<GetSharesRequest<DaShare>>,
) -> Response
where
    DaShare: Share + 'static,
    <DaShare as Share>::BlobId: Clone + Send + Sync + 'static,
    <DaShare as Share>::ShareIndex: Serialize + DeserializeOwned + Hash + Eq + Send + Sync,
    <DaShare as Share>::LightShare: Serialize + DeserializeOwned + Send + Sync + 'static,
    DaStorageConverter: DaConverter<RocksBackend, Share = DaShare> + Send + Sync + 'static,
    HttpStorageAdapter: StorageAdapter<RuntimeServiceId> + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<StorageService<RocksBackend, RuntimeServiceId>>,
{
    let relay = match get_relay_or_500(&handle).await {
        Ok(relay) => relay,
        Err(error_response) => return error_response,
    };
    match HttpStorageAdapter::get_shares::<DaStorageConverter, DaShare>(
        relay,
        request.blob_id,
        request.requested_shares,
        request.filter_shares,
        request.return_available,
    )
    .await
    {
        Ok(shares) => {
            let body = Body::from_stream(shares);
            match Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(body)
            {
                Ok(response) => response,
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = paths::DA_BALANCER_STATS,
    responses(
        (status = 200, description = "Get balancer stats", body = String),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn balancer_stats<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Backend::Message: BalancerMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::balancer_stats::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
        RuntimeServiceId,
    >(&handle))
}

#[utoipa::path(
    get,
    path = paths::DA_BALANCER_STATS,
    responses(
        (status = 200, description = "Get monitor stats", body = String),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn monitor_stats<
    Backend,
    Membership,
    MembershipAdapter,
    MembershipStorage,
    ApiAdapter,
    SdpAdapter,
    RuntimeServiceId,
>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
) -> Response
where
    Backend: NetworkBackend<RuntimeServiceId> + Send + 'static,
    Backend::Message: MonitorMessageFactory,
    Membership: MembershipHandler + Clone + Send + Sync + 'static,
    Membership::Id: Send + Sync + 'static,
    Membership::NetworkId: Send + Sync + 'static,
    ApiAdapter: ApiAdapterTrait + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Display
        + 'static
        + AsServiceId<
            NetworkService<
                Backend,
                Membership,
                MembershipAdapter,
                MembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >,
{
    make_request_and_return_response!(da::monitor_stats::<
        Backend,
        Membership,
        MembershipAdapter,
        MembershipStorage,
        ApiAdapter,
        SdpAdapter,
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
    StorageAdapter: tx_service::storage::MempoolStorageAdapter<
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
        (status = 200, description = "Post declaration to SDP service", body = nomos_core::sdp::DeclarationId),
        (status = 500, description = "Internal server error", body = String),
    )
)]
pub async fn post_declaration<MempoolAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration): Json<nomos_core::sdp::DeclarationMessage>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<nomos_sdp::SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(nomos_api::http::sdp::post_declaration_handler::<
        MempoolAdapter,
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
pub async fn post_activity<MempoolAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(metadata): Json<nomos_core::sdp::ActivityMetadata>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<nomos_sdp::SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(nomos_api::http::sdp::post_activity_handler::<
        MempoolAdapter,
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
pub async fn post_withdrawal<MempoolAdapter, RuntimeServiceId>(
    State(handle): State<OverwatchHandle<RuntimeServiceId>>,
    Json(declaration_id): Json<nomos_core::sdp::DeclarationId>,
) -> Response
where
    MempoolAdapter: SdpMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + 'static
        + AsServiceId<nomos_sdp::SdpService<MempoolAdapter, RuntimeServiceId>>,
{
    make_request_and_return_response!(nomos_api::http::sdp::post_withdrawal_handler::<
        MempoolAdapter,
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
    StorageBackend: nomos_storage::backends::StorageBackend + Send + Sync + 'static, /* TODO: StorageChainApi */
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
    StorageBackend: nomos_storage::backends::StorageBackend + Send + Sync + 'static,
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
    use key_management_system_service::keys::ZkPublicKey;

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
    pub async fn get_balance<
        WalletService,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
        MempoolStorageAdapter,
        TimeBackend,
        RuntimeServiceId,
    >(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Path(address): Path<ZkPublicKey>,
        Query(query): Query<TipQuery>,
    ) -> Response
    where
        WalletService: WalletServiceData + 'static,
        SamplingBackend: DaSamplingServiceBackend<BlobId = BlobId> + Send,
        SamplingBackend::Settings: Clone,
        SamplingBackend::Share: Debug + 'static,
        SamplingBackend::BlobId: Debug + 'static,
        SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
        SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
        MempoolStorageAdapter: tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Key = <SignedMantleTx as Transaction>::Hash,
                Item = SignedMantleTx,
            > + Clone
            + 'static,
        MempoolStorageAdapter::Error: Debug,
        TimeBackend: nomos_time::backends::TimeBackend,
        TimeBackend::Settings: Clone + Send + Sync,
        RuntimeServiceId: Debug
            + Send
            + Sync
            + Display
            + 'static
            + AsServiceId<WalletService>
            + AsServiceId<Cryptarchia<RuntimeServiceId>>,
    {
        let wallet_api = {
            let wallet_relay = match get_relay_or_500::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error_response) => return error_response,
            };
            WalletApi::<WalletService, RuntimeServiceId>::new(wallet_relay)
        };
        let tip = {
            if let Some(tip) = query.tip {
                tip
            } else if let Ok(info) = consensus::cryptarchia_info(&handle).await {
                info.tip
            } else {
                error!(
                    "Failed to get cryptarchia info: It wasn't provided in the query and couldn't be retrieved from the consensus service."
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    String::from("Couldn't retrieve a valid tip"),
                )
                    .into_response();
            }
        };

        let balance = wallet_api.get_balance(tip, address).await;
        match balance {
            Ok(Some(balance)) => WalletBalanceResponseBody {
                tip,
                balance,
                address,
            }
            .into_response(),
            Ok(None) => (
                StatusCode::NOT_FOUND,
                "The requested address could not be found in the wallet.",
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
    pub async fn post_transactions_transfer_funds<
        WalletService,
        StorageBackend,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingStorage,
        MempoolStorageAdapter,
        TimeBackend,
        RuntimeServiceId,
    >(
        State(handle): State<OverwatchHandle<RuntimeServiceId>>,
        Json(body): Json<WalletTransferFundsRequestBody>,
    ) -> Response
    where
        WalletService: WalletServiceData + 'static,
        StorageBackend: nomos_storage::backends::StorageBackend + Send + Sync + 'static,
        SamplingBackend: DaSamplingServiceBackend<BlobId = BlobId> + Send,
        SamplingBackend::Settings: Clone,
        SamplingBackend::Share: Debug + 'static,
        SamplingBackend::BlobId: Debug + 'static,
        SamplingNetworkAdapter: nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId>,
        SamplingStorage: nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId>,
        MempoolStorageAdapter: tx_service::storage::MempoolStorageAdapter<
                RuntimeServiceId,
                Key = <SignedMantleTx as Transaction>::Hash,
                Item = SignedMantleTx,
            > + Clone
            + 'static,
        MempoolStorageAdapter::Error: Debug,
        TimeBackend: nomos_time::backends::TimeBackend,
        TimeBackend::Settings: Clone + Send + Sync,
        RuntimeServiceId: Debug
            + Send
            + Sync
            + Display
            + 'static
            + AsServiceId<WalletService>
            + AsServiceId<StorageService<StorageBackend, RuntimeServiceId>>
            + AsServiceId<Cryptarchia<RuntimeServiceId>>,
    {
        let wallet_api = {
            let wallet_relay = match get_relay_or_500::<WalletService, _>(&handle).await {
                Ok(relay) => relay,
                Err(error_response) => return error_response,
            };
            WalletApi::<WalletService, RuntimeServiceId>::new(wallet_relay)
        };

        let tip = {
            if let Some(tip) = body.tip {
                tip
            } else if let Ok(info) = consensus::cryptarchia_info(&handle).await {
                info.tip
            } else {
                error!(
                    "Failed to get cryptarchia info: It wasn't provided in the query and couldn't be retrieved from the consensus service."
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    String::from("Couldn't retrieve a valid tip"),
                )
                    .into_response();
            }
        };

        let transfer_funds = wallet_api
            .transfer_funds(
                tip,
                body.change_public_key,
                body.funding_public_keys,
                body.recipient_public_key,
                body.amount,
            )
            .await;

        match transfer_funds {
            Ok(transaction) => WalletTransferFundsResponseBody::from(transaction).into_response(),
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
        }
    }
}
