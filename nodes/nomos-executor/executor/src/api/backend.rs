#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use std::{
    error::Error,
    fmt::{Debug, Display},
    hash::Hash,
    time::Duration,
};

use axum::{
    Router,
    http::{
        HeaderValue,
        header::{CONTENT_TYPE, USER_AGENT},
    },
    routing,
};
use nomos_api::{
    Backend,
    http::{
        consensus::Cryptarchia,
        da::{DaDispersal, DaVerifier},
        storage,
    },
};
use nomos_core::{
    da::{
        DaVerifier as CoreDaVerifier,
        blob::{LightShare, Share, info::DispersedBlobInfo, metadata},
    },
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction},
};
use nomos_da_network_core::SubnetworkId;
use nomos_da_network_service::{
    backends::libp2p::executor::DaNetworkExecutorBackend, membership::MembershipAdapter,
    sdp::SdpAdapter as SdpAdapterTrait, storage::MembershipStorageAdapter,
};
use nomos_da_sampling::{DaSamplingService, backend::DaSamplingServiceBackend};
use nomos_da_verifier::{backend::VerifierBackend, mempool::DaMempoolAdapter};
pub use nomos_http_api_common::settings::AxumBackendSettings;
use nomos_http_api_common::{paths, utils::create_rate_limit_layer};
use nomos_libp2p::PeerId;
use nomos_node::{
    RocksBackend,
    api::handlers::{
        add_share, add_tx, balancer_stats, blacklisted_peers, block, block_peer,
        cryptarchia_headers, cryptarchia_info, da_get_commitments, da_get_light_share,
        da_get_shares, da_get_storage_commitments, libp2p_info, mantle_metrics, mantle_status,
        monitor_stats, post_activity, post_declaration, post_withdrawal, unblock_peer,
    },
};
use nomos_sdp::adapters::mempool::SdpMempoolAdapter;
use nomos_storage::{StorageService, api::da};
use overwatch::{DynError, overwatch::handle::OverwatchHandle, services::AsServiceId};
use serde::{Serialize, de::DeserializeOwned};
use services_utils::wait_until_services_are_ready;
use subnetworks_assignations::MembershipHandler;
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tx_service::{
    MempoolMetrics, TxMempoolService, backend::Mempool, tx::service::openapi::Status,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use super::handlers::disperse_data;

type DaStorageBackend = RocksBackend;
type DaStorageService<RuntimeServiceId> = StorageService<DaStorageBackend, RuntimeServiceId>;

pub struct AxumBackend<
    DaShare,
    DaBlobInfo,
    Memebership,
    DaMembershipAdapter,
    DaMembershipStorage,
    DaVerifiedBlobInfo,
    DaVerifierBackend,
    DaVerifierNetwork,
    DaVerifierStorage,
    DaStorageConverter,
    DispersalBackend,
    DispersalNetworkAdapter,
    Metadata,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingMempoolAdapter,
    SamplingStorage,
    VerifierMempoolAdapter,
    TimeBackend,
    ApiAdapter,
    SdpAdapter,
    HttpStorageAdapter,
    MempoolStorageAdapter,
    SdpMempool,
> {
    settings: AxumBackendSettings,
    #[expect(clippy::allow_attributes_without_reason)]
    #[expect(clippy::type_complexity)]
    _phantom: core::marker::PhantomData<(
        DaShare,
        DaBlobInfo,
        Memebership,
        DaMembershipAdapter,
        DaMembershipStorage,
        DaVerifiedBlobInfo,
        DaVerifierBackend,
        DaVerifierNetwork,
        DaVerifierStorage,
        DaStorageConverter,
        DispersalBackend,
        DispersalNetworkAdapter,
        Metadata,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingMempoolAdapter,
        SamplingStorage,
        VerifierMempoolAdapter,
        TimeBackend,
        ApiAdapter,
        SdpAdapter,
        HttpStorageAdapter,
        MempoolStorageAdapter,
        SdpMempool,
    )>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
    ),
    components(
        schemas(Status, MempoolMetrics)
    ),
    tags(
        (name = "da", description = "data availibility related APIs")
    )
)]
struct ApiDoc;

#[async_trait::async_trait]
impl<
    DaShare,
    DaBlobInfo,
    Membership,
    DaMembershipAdapter,
    DaMembershipStorage,
    DaVerifiedBlobInfo,
    DaVerifierBackend,
    DaVerifierNetwork,
    DaVerifierStorage,
    DaStorageConverter,
    DispersalBackend,
    DispersalNetworkAdapter,
    Metadata,
    SamplingBackend,
    SamplingNetworkAdapter,
    SamplingMempoolAdapter,
    SamplingStorage,
    VerifierMempoolAdapter,
    TimeBackend,
    ApiAdapter,
    SdpAdapter,
    StorageAdapter,
    MempoolStorageAdapter,
    SdpMempool,
    RuntimeServiceId,
> Backend<RuntimeServiceId>
    for AxumBackend<
        DaShare,
        DaBlobInfo,
        Membership,
        DaMembershipAdapter,
        DaMembershipStorage,
        DaVerifiedBlobInfo,
        DaVerifierBackend,
        DaVerifierNetwork,
        DaVerifierStorage,
        DaStorageConverter,
        DispersalBackend,
        DispersalNetworkAdapter,
        Metadata,
        SamplingBackend,
        SamplingNetworkAdapter,
        SamplingMempoolAdapter,
        SamplingStorage,
        VerifierMempoolAdapter,
        TimeBackend,
        ApiAdapter,
        SdpAdapter,
        StorageAdapter,
        MempoolStorageAdapter,
        SdpMempool,
    >
where
    DaShare: Share + Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <DaShare as Share>::BlobId: Clone + Serialize + DeserializeOwned + Send + Sync + 'static,
    <DaShare as Share>::ShareIndex:
        Clone + Serialize + DeserializeOwned + Hash + Eq + Send + Sync + 'static,
    <DaShare as Share>::SharesCommitments:
        Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    <DaShare as Share>::LightShare: LightShare<ShareIndex = <DaShare as Share>::ShareIndex>
        + Serialize
        + DeserializeOwned
        + Clone
        + Send
        + Sync
        + 'static,
    DaBlobInfo: DispersedBlobInfo<BlobId = [u8; 32]>
        + Clone
        + Debug
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static,
    <DaBlobInfo as DispersedBlobInfo>::BlobId: Clone + Send + Sync,
    Membership: MembershipHandler<NetworkId = SubnetworkId, Id = PeerId>
        + Clone
        + Debug
        + Send
        + Sync
        + 'static,
    DaVerifiedBlobInfo: DispersedBlobInfo<BlobId = [u8; 32]>
        + From<DaBlobInfo>
        + Eq
        + Debug
        + metadata::Metadata
        + Hash
        + Clone
        + Serialize
        + DeserializeOwned
        + Send
        + Sync
        + 'static,
    <DaVerifiedBlobInfo as DispersedBlobInfo>::BlobId: Debug + Clone + Ord + Hash,
    <DaVerifiedBlobInfo as metadata::Metadata>::AppId:
        AsRef<[u8]> + Clone + Serialize + DeserializeOwned + Send + Sync,
    <DaVerifiedBlobInfo as metadata::Metadata>::Index:
        AsRef<[u8]> + Clone + Serialize + DeserializeOwned + PartialOrd + Send + Sync,
    DaVerifierBackend: VerifierBackend + CoreDaVerifier<DaShare = DaShare> + Send + Sync + 'static,
    <DaVerifierBackend as VerifierBackend>::Settings: Clone,
    <DaVerifierBackend as CoreDaVerifier>::Error: Error,
    DaVerifierNetwork:
        nomos_da_verifier::network::NetworkAdapter<RuntimeServiceId> + Send + Sync + 'static,
    DaVerifierNetwork::Settings: Clone,
    DaVerifierStorage:
        nomos_da_verifier::storage::DaStorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    DaVerifierStorage::Settings: Clone,
    DaMembershipAdapter: MembershipAdapter + Send + Sync + 'static,
    DaMembershipStorage: MembershipStorageAdapter<PeerId, SubnetworkId> + Send + Sync + 'static,
    DaStorageConverter: da::DaConverter<DaStorageBackend, Share = DaShare, Tx = SignedMantleTx>
        + Send
        + Sync
        + 'static,
    DispersalBackend: nomos_da_dispersal::backend::DispersalBackend<NetworkAdapter = DispersalNetworkAdapter>
        + Send
        + Sync
        + 'static,
    DispersalBackend::BlobId: Serialize,
    DispersalBackend::Settings: Clone + Send + Sync,
    DispersalNetworkAdapter: nomos_da_dispersal::adapters::network::DispersalNetworkAdapter<
            SubnetworkId = Membership::NetworkId,
        > + Send
        + 'static,
    Metadata: DeserializeOwned + metadata::Metadata + Debug + Send + 'static,
    SamplingBackend: DaSamplingServiceBackend<BlobId = <DaVerifiedBlobInfo as DispersedBlobInfo>::BlobId>
        + Send
        + 'static,
    SamplingBackend::Settings: Clone,
    SamplingBackend::Share: Debug + 'static,
    SamplingBackend::BlobId: Debug + 'static,
    <DaShare as Share>::LightShare: LightShare<ShareIndex = <DaShare as Share>::ShareIndex>
        + Serialize
        + DeserializeOwned
        + Clone
        + Send
        + Sync
        + 'static,
    SamplingNetworkAdapter:
        nomos_da_sampling::network::NetworkAdapter<RuntimeServiceId> + Send + Sync + 'static,
    SamplingStorage:
        nomos_da_sampling::storage::DaStorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    VerifierMempoolAdapter: DaMempoolAdapter + Send + Sync + 'static,
    TimeBackend: nomos_time::backends::TimeBackend + Send + 'static,
    TimeBackend::Settings: Clone + Send + Sync,
    ApiAdapter: nomos_da_network_service::api::ApiAdapter + Send + Sync + 'static,
    SdpAdapter: SdpAdapterTrait<RuntimeServiceId> + Send + Sync + 'static,
    StorageAdapter: storage::StorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    SdpMempool: SdpMempoolAdapter + Send + Sync + 'static,
    MempoolStorageAdapter: tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Key = <SignedMantleTx as Transaction>::Hash,
            Item = SignedMantleTx,
        > + Send
        + Sync
        + Clone
        + 'static,
    MempoolStorageAdapter::Error: Debug,
    SamplingMempoolAdapter: nomos_da_sampling::mempool::DaMempoolAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + Clone
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<
            DaVerifier<
                DaShare,
                DaVerifierNetwork,
                DaVerifierBackend,
                DaStorageConverter,
                VerifierMempoolAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<
            nomos_da_network_service::NetworkService<
                DaNetworkExecutorBackend<Membership>,
                Membership,
                DaMembershipAdapter,
                DaMembershipStorage,
                ApiAdapter,
                SdpAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<
            nomos_network::NetworkService<
                nomos_network::backends::libp2p::Libp2p,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<DaStorageService<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                tx_service::network::adapters::libp2p::Libp2pAdapter<
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedMantleTx,
                    <SignedMantleTx as Transaction>::Hash,
                    MempoolStorageAdapter,
                    RuntimeServiceId,
                >,
                MempoolStorageAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<
            DaDispersal<DispersalBackend, DispersalNetworkAdapter, Membership, RuntimeServiceId>,
        >
        + AsServiceId<
            DaSamplingService<
                SamplingBackend,
                SamplingNetworkAdapter,
                SamplingStorage,
                SamplingMempoolAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<nomos_sdp::SdpService<SdpMempool, RuntimeServiceId>>,
{
    type Error = std::io::Error;
    type Settings = AxumBackendSettings;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            settings,
            _phantom: core::marker::PhantomData,
        })
    }

    async fn wait_until_ready(
        &mut self,
        overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Result<(), DynError> {
        wait_until_services_are_ready!(
            &overwatch_handle,
            Some(Duration::from_secs(60)),
            Cryptarchia<_>,
            DaVerifier<_, _, _, _, _, _>,
            nomos_da_network_service::NetworkService<_, _, _,_, _, _, _>,
            nomos_network::NetworkService<_, _>,
            DaStorageService<_>,
            TxMempoolService<_, _, _, _,>,
            DaDispersal<_, _, _, _>
        )
        .await
    }

    #[expect(clippy::too_many_lines, reason = "TODO: Address this at some point.")]
    async fn serve(self, handle: OverwatchHandle<RuntimeServiceId>) -> Result<(), Self::Error> {
        let mut builder = CorsLayer::new();
        if self.settings.cors_origins.is_empty() {
            builder = builder.allow_origin(Any);
        }

        for origin in &self.settings.cors_origins {
            builder = builder.allow_origin(
                origin
                    .as_str()
                    .parse::<HeaderValue>()
                    .expect("fail to parse origin"),
            );
        }

        let app = Router::new()
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
            .route(
                paths::MANTLE_METRICS,
                routing::get(mantle_metrics::<MempoolStorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::MANTLE_STATUS,
                routing::post(mantle_status::<MempoolStorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::CRYPTARCHIA_INFO,
                routing::get(cryptarchia_info::<RuntimeServiceId>),
            )
            .route(
                paths::CRYPTARCHIA_HEADERS,
                routing::get(cryptarchia_headers::<RuntimeServiceId>),
            )
            .route(
                paths::DA_ADD_SHARE,
                routing::post(
                    add_share::<
                        DaShare,
                        DaVerifierNetwork,
                        DaVerifierBackend,
                        DaStorageConverter,
                        VerifierMempoolAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_BLOCK_PEER,
                routing::post(
                    block_peer::<
                        DaNetworkExecutorBackend<Membership>,
                        Membership,
                        DaMembershipAdapter,
                        DaMembershipStorage,
                        ApiAdapter,
                        SdpAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_UNBLOCK_PEER,
                routing::post(
                    unblock_peer::<
                        DaNetworkExecutorBackend<Membership>,
                        Membership,
                        DaMembershipAdapter,
                        DaMembershipStorage,
                        ApiAdapter,
                        SdpAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_BLACKLISTED_PEERS,
                routing::get(
                    blacklisted_peers::<
                        DaNetworkExecutorBackend<Membership>,
                        Membership,
                        DaMembershipAdapter,
                        DaMembershipStorage,
                        ApiAdapter,
                        SdpAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(paths::NETWORK_INFO, routing::get(libp2p_info))
            .route(
                paths::STORAGE_BLOCK,
                routing::post(block::<StorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::MEMPOOL_ADD_TX,
                routing::post(add_tx::<MempoolStorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::DISPERSE_DATA,
                routing::post(
                    disperse_data::<
                        DispersalBackend,
                        DispersalNetworkAdapter,
                        Membership,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_GET_SHARES_COMMITMENTS,
                routing::post(
                    da_get_commitments::<
                        DaVerifiedBlobInfo::BlobId,
                        SamplingBackend,
                        SamplingNetworkAdapter,
                        SamplingStorage,
                        SamplingMempoolAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_GET_STORAGE_SHARES_COMMITMENTS,
                routing::get(
                    da_get_storage_commitments::<
                        DaStorageConverter,
                        StorageAdapter,
                        DaShare,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_GET_LIGHT_SHARE,
                routing::get(
                    da_get_light_share::<
                        DaStorageConverter,
                        StorageAdapter,
                        DaShare,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_GET_SHARES,
                routing::get(
                    da_get_shares::<DaStorageConverter, StorageAdapter, DaShare, RuntimeServiceId>,
                ),
            )
            .route(
                paths::DA_BALANCER_STATS,
                routing::get(
                    balancer_stats::<
                        DaNetworkExecutorBackend<Membership>,
                        Membership,
                        DaMembershipAdapter,
                        DaMembershipStorage,
                        ApiAdapter,
                        SdpAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::DA_MONITOR_STATS,
                routing::get(
                    monitor_stats::<
                        DaNetworkExecutorBackend<Membership>,
                        Membership,
                        DaMembershipAdapter,
                        DaMembershipStorage,
                        ApiAdapter,
                        SdpAdapter,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                paths::SDP_POST_DECLARATION,
                routing::post(post_declaration::<SdpMempool, RuntimeServiceId>),
            )
            .route(
                paths::SDP_POST_ACTIVITY,
                routing::post(post_activity::<SdpMempool, RuntimeServiceId>),
            )
            .route(
                paths::SDP_POST_WITHDRAWAL,
                routing::post(post_withdrawal::<SdpMempool, RuntimeServiceId>),
            )
            .with_state(handle.clone())
            .layer(TimeoutLayer::new(self.settings.timeout))
            .layer(RequestBodyLimitLayer::new(self.settings.max_body_size))
            .layer(ConcurrencyLimitLayer::new(
                self.settings.max_concurrent_requests,
            ))
            .layer(create_rate_limit_layer(&self.settings))
            .layer(TraceLayer::new_for_http());

        let cors_layer = builder
            .allow_headers(vec![CONTENT_TYPE, USER_AGENT])
            .allow_methods(Any);

        let app = app.layer(cors_layer.clone());

        #[cfg(feature = "profiling")]
        let app = {
            let pprof_routes = nomos_http_api_common::pprof::create_pprof_router()
                .layer(TraceLayer::new_for_http())
                .layer(cors_layer);

            app.merge(pprof_routes)
        };

        let listener = TcpListener::bind(&self.settings.address)
            .await
            .expect("Failed to bind address");

        let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, app).await
    }
}
