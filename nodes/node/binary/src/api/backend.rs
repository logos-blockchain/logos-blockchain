#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
};

use axum::{
    Router,
    http::{
        HeaderValue,
        header::{CONTENT_TYPE, USER_AGENT},
    },
    routing,
};
use http::StatusCode;
use lb_api_service::{Backend, http::consensus::Cryptarchia};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_leader_service::api::ChainLeaderServiceData;
use lb_chain_service::CryptarchiaConsensus;
use lb_core::{
    header::HeaderId,
    mantle::{
        SignedOps, ledger::verification_mode::StandardMode, traits::Hashable,
        transactions::states::Preverified,
    },
};
use lb_http_api_common::metrics::http_metrics_middleware;
pub use lb_http_api_common::settings::AxumBackendSettings;
use lb_sdp_service::{
    mempool::SdpMempoolAdapter, state::SdpStateStorage as SdpStateStorageTrait,
    wallet::SdpWalletAdapter,
};
use lb_storage_service::{StorageService, backends::rocksdb::RocksBackend};
use lb_tx_service::{TxMempoolService, backend::Mempool};
use overwatch::{overwatch::handle::OverwatchHandle, services::AsServiceId};
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::Level as TracingLevel;
use utoipa::OpenApi as _;
use utoipa_swagger_ui::SwaggerUi;

use super::handlers::{
    add_tx, blend_info, blend_pending_transactions, blend_tx, block, block_events,
    blocks_range_stream, blocks_stream, cryptarchia_headers, cryptarchia_info,
    cryptarchia_lib_stream, dial_peer, get_gas_prices, get_sdp_declarations, get_sdp_snapshot,
    immutable_blocks, libp2p_info, mantle_metrics, mantle_status, mempool_view, time_info,
    transaction, version, wallet,
};
use crate::{
    BlendService, PoWService, TracingService, WalletService,
    api::{
        handlers::{
            blend_join_network, channel, channel_deposit, leader_claim, post_activity,
            post_declaration, post_set_declaration_id, post_withdrawal, pow_claim,
            pow_claimable_rewards, pow_start_auto_claim, pow_start_mining, pow_stop_auto_claim,
            pow_stop_mining,
        },
        openapi::ApiDoc,
        routes::api_routes,
        tracing::reload_tracing_filter,
    },
};

/// Builds the axum router from the shared route table.
///
/// Only the `$handler` half of each row is used; the `OpenAPI` half is matched
/// and discarded. Expanded inside `AxumBackend::serve`, where the handler type
/// parameters are in scope.
macro_rules! build_router {
    ($( $method:ident $path:expr => $doc:path, $handler:expr ; )*) => {
        Router::new()$(.route($path, routing::$method($handler)))*
    };
}

pub(crate) type BlockStorageBackend = RocksBackend;
type BlockStorageService<RuntimeServiceId> = StorageService<BlockStorageBackend, RuntimeServiceId>;

pub struct AxumBackend<
    TimeBackend,
    HttpStorageAdapter,
    MempoolStorageAdapter,
    SdpMempool,
    SdpWallet,
    SdpStateStorage,
    ChainLeader,
> {
    settings: AxumBackendSettings,
    _phantom: PhantomData<(
        TimeBackend,
        HttpStorageAdapter,
        MempoolStorageAdapter,
        SdpMempool,
        SdpWallet,
        SdpStateStorage,
        ChainLeader,
    )>,
}

#[async_trait::async_trait]
impl<
    TimeBackend,
    StorageAdapter,
    MempoolStorageAdapter,
    SdpMempool,
    SdpWallet,
    SdpStateStorage,
    ChainLeader,
    RuntimeServiceId,
> Backend<RuntimeServiceId>
    for AxumBackend<
        TimeBackend,
        StorageAdapter,
        MempoolStorageAdapter,
        SdpMempool,
        SdpWallet,
        SdpStateStorage,
        ChainLeader,
    >
where
    TimeBackend: lb_time_service::backends::TimeBackend + Send + 'static,
    TimeBackend::Settings: Clone + Send + Sync,
    StorageAdapter:
        lb_api_service::http::storage::StorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    MempoolStorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedOps<Preverified, StandardMode>,
            Key = <SignedOps<Preverified, StandardMode> as Hashable>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    MempoolStorageAdapter::Error: Debug,
    SdpMempool: SdpMempoolAdapter + Send + Sync + 'static,
    SdpWallet: SdpWalletAdapter + Send + Sync + 'static,
    ChainLeader: ChainLeaderServiceData,
    SdpStateStorage: SdpStateStorageTrait<RuntimeServiceId> + Send + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + Clone
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<crate::TimeService>
        + AsServiceId<BlockBroadcastService<RuntimeServiceId>>
        + AsServiceId<
            lb_network_service::NetworkService<
                lb_network_service::backends::libp2p::Libp2p,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<BlockStorageService<RuntimeServiceId>>
        + AsServiceId<
            StorageService<
                <MempoolStorageAdapter as lb_tx_service::storage::MempoolStorageAdapter<
                    RuntimeServiceId,
                >>::Backend,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<
            TxMempoolService<
                lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
                    SignedOps<Preverified, StandardMode>,
                    <SignedOps<Preverified, StandardMode> as Hashable>::Hash,
                    RuntimeServiceId,
                >,
                Mempool<
                    HeaderId,
                    SignedOps<Preverified, StandardMode>,
                    <SignedOps<Preverified, StandardMode> as Hashable>::Hash,
                    MempoolStorageAdapter,
                    RuntimeServiceId,
                >,
                MempoolStorageAdapter,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<
            lb_sdp_service::SdpService<
                SdpMempool,
                SdpWallet,
                Cryptarchia<RuntimeServiceId>,
                SdpStateStorage,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<WalletService>
        + AsServiceId<ChainLeader>
        + AsServiceId<BlendService>
        + AsServiceId<PoWService>
        + AsServiceId<TracingService>,
{
    type Error = std::io::Error;
    type Settings = AxumBackendSettings;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            settings,
            _phantom: PhantomData,
        })
    }

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

        let app = api_routes!(build_router)
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

        let app = app
            .with_state(handle.clone())
            .layer(axum::middleware::from_fn(http_metrics_middleware))
            .layer(axum::extract::DefaultBodyLimit::max(
                self.settings.max_body_size,
            ))
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                self.settings.timeout,
            ))
            .layer(RequestBodyLimitLayer::new(self.settings.max_body_size))
            .layer(ConcurrencyLimitLayer::new(
                self.settings.max_concurrent_requests,
            ))
            .layer(
                TraceLayer::new_for_http()
                    .on_request(DefaultOnRequest::new().level(TracingLevel::TRACE))
                    .on_response(DefaultOnResponse::new().level(TracingLevel::TRACE)),
            );

        let cors_layer = builder
            .allow_headers(vec![CONTENT_TYPE, USER_AGENT])
            .allow_methods(Any);

        let app = app.layer(cors_layer.clone());

        #[cfg(feature = "profiling")]
        let app = {
            let pprof_routes = lb_http_api_common::pprof::create_pprof_router()
                .layer(
                    TraceLayer::new_for_http()
                        .on_request(DefaultOnRequest::new().level(TracingLevel::TRACE))
                        .on_response(DefaultOnResponse::new().level(TracingLevel::TRACE)),
                )
                .layer(cors_layer);

            app.merge(pprof_routes)
        };

        let listener = TcpListener::bind(&self.settings.address)
            .await
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("Failed to bind to address {}: {}", self.settings.address, e),
                )
            })?;

        let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, app).await
    }
}
