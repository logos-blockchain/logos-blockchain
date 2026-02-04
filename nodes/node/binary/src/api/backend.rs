#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use std::{
    fmt::{Debug, Display},
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
use lb_api_service::{Backend, http::consensus::Cryptarchia};
use lb_chain_broadcast_service::BlockBroadcastService;
use lb_chain_service::CryptarchiaConsensus;
use lb_core::{
    header::HeaderId,
    mantle::{SignedMantleTx, Transaction},
};
pub use lb_http_api_common::settings::AxumBackendSettings;
use lb_http_api_common::{paths, utils::create_rate_limit_layer};
use lb_sdp_service::{mempool::SdpMempoolAdapter, wallet::SdpWalletAdapter};
use lb_services_utils::wait_until_services_are_ready;
use lb_storage_service::{StorageService, backends::rocksdb::RocksBackend};
use lb_tx_service::{
    MempoolMetrics, TxMempoolService, backend::Mempool, tx::service::openapi::Status,
};
use overwatch::{DynError, overwatch::handle::OverwatchHandle, services::AsServiceId};
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use super::handlers::{
    add_tx, block, blocks, blocks_stream, cryptarchia_headers, cryptarchia_info,
    cryptarchia_lib_stream, libp2p_info, mantle_metrics, mantle_status, wallet,
};
use crate::{
    WalletService,
    api::handlers::{post_activity, post_declaration, post_withdrawal},
};

pub(crate) type BlockStorageBackend = RocksBackend;
type BlockStorageService<RuntimeServiceId> = StorageService<BlockStorageBackend, RuntimeServiceId>;

pub struct AxumBackend<
    TimeBackend,
    HttpStorageAdapter,
    MempoolStorageAdapter,
    SdpMempool,
    SdpWallet,
> {
    settings: AxumBackendSettings,
    _time_backend: core::marker::PhantomData<TimeBackend>,
    _storage_adapter: core::marker::PhantomData<HttpStorageAdapter>,
    _mempool_storage_adapter: core::marker::PhantomData<MempoolStorageAdapter>,
    _sdp_mempool_adapter: core::marker::PhantomData<SdpMempool>,
    _sdp_wallet_adapter: core::marker::PhantomData<SdpWallet>,
}

#[derive(OpenApi)]
#[openapi(paths(), components(schemas(Status, MempoolMetrics)), tags())]
struct ApiDoc;

#[async_trait::async_trait]
impl<TimeBackend, StorageAdapter, MempoolStorageAdapter, SdpMempool, SdpWallet, RuntimeServiceId>
    Backend<RuntimeServiceId>
    for AxumBackend<TimeBackend, StorageAdapter, MempoolStorageAdapter, SdpMempool, SdpWallet>
where
    TimeBackend: lb_time_service::backends::TimeBackend + Send + 'static,
    TimeBackend::Settings: Clone + Send + Sync,
    StorageAdapter:
        lb_api_service::http::storage::StorageAdapter<RuntimeServiceId> + Send + Sync + 'static,
    MempoolStorageAdapter: lb_tx_service::storage::MempoolStorageAdapter<
            RuntimeServiceId,
            Item = SignedMantleTx,
            Key = <SignedMantleTx as Transaction>::Hash,
        > + Send
        + Sync
        + Clone
        + 'static,
    MempoolStorageAdapter::Error: Debug,
    SdpMempool: SdpMempoolAdapter + Send + Sync + 'static,
    SdpWallet: SdpWalletAdapter + Send + Sync + 'static,
    RuntimeServiceId: Debug
        + Sync
        + Send
        + Display
        + Clone
        + 'static
        + AsServiceId<Cryptarchia<RuntimeServiceId>>
        + AsServiceId<BlockBroadcastService<RuntimeServiceId>>
        + AsServiceId<
            lb_network_service::NetworkService<
                lb_network_service::backends::libp2p::Libp2p,
                RuntimeServiceId,
            >,
        >
        + AsServiceId<BlockStorageService<RuntimeServiceId>>
        + AsServiceId<
            TxMempoolService<
                lb_tx_service::network::adapters::libp2p::Libp2pAdapter<
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
        + AsServiceId<lb_sdp_service::SdpService<SdpMempool, SdpWallet, RuntimeServiceId>>
        + AsServiceId<WalletService>,
{
    type Error = std::io::Error;
    type Settings = AxumBackendSettings;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self {
            settings,
            _time_backend: core::marker::PhantomData,
            _storage_adapter: core::marker::PhantomData,
            _mempool_storage_adapter: core::marker::PhantomData,
            _sdp_mempool_adapter: core::marker::PhantomData,
            _sdp_wallet_adapter: core::marker::PhantomData,
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
            lb_network_service::NetworkService<_, _>,
            BlockStorageService<_>,
            TxMempoolService<_, _, _,  _>
        )
        .await?;
        Ok(())
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
                paths::CRYPTARCHIA_LIB_STREAM,
                routing::get(cryptarchia_lib_stream::<RuntimeServiceId>),
            )
            .route(
                paths::NETWORK_INFO,
                routing::get(libp2p_info::<RuntimeServiceId>),
            )
            .route(
                paths::STORAGE_BLOCK,
                routing::post(block::<StorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::MEMPOOL_ADD_TX,
                routing::post(add_tx::<MempoolStorageAdapter, RuntimeServiceId>),
            )
            .route(
                paths::SDP_POST_DECLARATION,
                routing::post(post_declaration::<SdpMempool, SdpWallet, RuntimeServiceId>),
            )
            .route(
                paths::SDP_POST_ACTIVITY,
                routing::post(post_activity::<SdpMempool, SdpWallet, RuntimeServiceId>),
            )
            .route(
                paths::SDP_POST_WITHDRAWAL,
                routing::post(post_withdrawal::<SdpMempool, SdpWallet, RuntimeServiceId>),
            )
            .route(
                paths::wallet::BALANCE,
                routing::get(wallet::get_balance::<WalletService, _>),
            )
            .route(
                paths::wallet::TRANSACTIONS_TRANSFER_FUNDS,
                routing::post(
                    wallet::post_transactions_transfer_funds::<
                        WalletService,
                        MempoolStorageAdapter,
                        _,
                    >,
                ),
            );

        let app = app.route(
            paths::BLOCKS_STREAM,
            routing::get(
                blocks_stream::<
                    BlockStorageBackend,
                    CryptarchiaConsensus<_, _, _, _>,
                    RuntimeServiceId,
                >,
            ),
        );

        let app = app.route(
            paths::BLOCKS,
            routing::get(blocks::<BlockStorageBackend, RuntimeServiceId>),
        );

        let app = app
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
            let pprof_routes = lb_http_api_common::pprof::create_pprof_router()
                .layer(TraceLayer::new_for_http())
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
