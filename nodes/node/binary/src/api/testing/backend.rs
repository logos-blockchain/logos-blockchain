use std::fmt::{Debug, Display};

use axum::{
    Router,
    http::{
        HeaderValue,
        header::{CONTENT_TYPE, USER_AGENT},
    },
    routing::{get, post},
};
use lb_api_service::Backend;
use lb_da_network_service::backends::libp2p::validator::DaNetworkValidatorBackend;
use lb_da_sampling_service::{
    backend::kzgrs::KzgrsSamplingBackend,
    network::adapters::validator::Libp2pAdapter as SamplingLibp2pAdapter,
    storage::adapters::rocksdb::{
        RocksAdapter as SamplingStorageAdapter, converter::DaStorageConverter,
    },
};
use lb_http_api_common::{
    paths::{DA_GET_MEMBERSHIP, DA_HISTORIC_SAMPLING, MANTLE_SDP_DECLARATIONS},
    utils::create_rate_limit_layer,
};
use lb_kzgrs_backend::common::share::DaShare;
pub use lb_network_service::backends::libp2p::Libp2p as NetworkBackend;
use overwatch::{DynError, overwatch::handle::OverwatchHandle, services::AsServiceId};
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use crate::{
    DaMembershipStorage, DaNetworkApiAdapter, LogosBlockchainDaMembership,
    api::{
        backend::AxumBackendSettings,
        testing::handlers::{da_get_membership, da_historic_sampling, get_sdp_declarations},
    },
    generic_services::{
        self, DaMembershipAdapter, SamplingMempoolAdapter, SdpService, SdpServiceAdapterGeneric,
    },
};
pub struct TestAxumBackend {
    settings: AxumBackendSettings,
}

type TestDaNetworkService<RuntimeServiceId> = lb_da_network_service::NetworkService<
    DaNetworkValidatorBackend<LogosBlockchainDaMembership>,
    LogosBlockchainDaMembership,
    DaMembershipAdapter<RuntimeServiceId>,
    DaMembershipStorage,
    DaNetworkApiAdapter,
    SdpServiceAdapterGeneric<RuntimeServiceId>,
    RuntimeServiceId,
>;

type TestDaSamplingService<RuntimeServiceId> = generic_services::DaSamplingService<
    SamplingLibp2pAdapter<
        LogosBlockchainDaMembership,
        DaMembershipAdapter<RuntimeServiceId>,
        DaMembershipStorage,
        DaNetworkApiAdapter,
        SdpServiceAdapterGeneric<RuntimeServiceId>,
        RuntimeServiceId,
    >,
    RuntimeServiceId,
>;

type TestCryptarchiaService<RuntimeServiceId> =
    generic_services::CryptarchiaService<RuntimeServiceId>;

pub(super) type TestHttpCryptarchiaService<RuntimeServiceId> =
    lb_api_service::http::consensus::Cryptarchia<RuntimeServiceId>;

#[async_trait::async_trait]
impl<RuntimeServiceId> Backend<RuntimeServiceId> for TestAxumBackend
where
    RuntimeServiceId: Sync
        + Send
        + Display
        + Debug
        + Clone
        + 'static
        + AsServiceId<TestDaNetworkService<RuntimeServiceId>>
        + AsServiceId<TestDaSamplingService<RuntimeServiceId>>
        + AsServiceId<TestCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<TestHttpCryptarchiaService<RuntimeServiceId>>
        + AsServiceId<SdpService<RuntimeServiceId>>
        + AsServiceId<generic_services::TxMempoolService<RuntimeServiceId>>,
{
    type Error = std::io::Error;
    type Settings = AxumBackendSettings;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self { settings })
    }

    async fn wait_until_ready(
        &mut self,
        _overwatch_handle: OverwatchHandle<RuntimeServiceId>,
    ) -> Result<(), DynError> {
        Ok(())
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

        // Simple router with ONLY testing endpoints
        let app = Router::new()
            .route(
                DA_GET_MEMBERSHIP,
                post(
                    da_get_membership::<
                        DaNetworkValidatorBackend<LogosBlockchainDaMembership>,
                        LogosBlockchainDaMembership,
                        DaMembershipAdapter<RuntimeServiceId>,
                        DaMembershipStorage,
                        DaNetworkApiAdapter,
                        SdpServiceAdapterGeneric<RuntimeServiceId>,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                DA_HISTORIC_SAMPLING,
                post(
                    da_historic_sampling::<
                        KzgrsSamplingBackend,
                        lb_da_sampling_service::network::adapters::validator::Libp2pAdapter<
                            LogosBlockchainDaMembership,
                            DaMembershipAdapter<RuntimeServiceId>,
                            DaMembershipStorage,
                            DaNetworkApiAdapter,
                            SdpServiceAdapterGeneric<RuntimeServiceId>,
                            RuntimeServiceId,
                        >,
                        SamplingStorageAdapter<DaShare, DaStorageConverter>,
                        SamplingMempoolAdapter<RuntimeServiceId>,
                        RuntimeServiceId,
                    >,
                ),
            )
            .route(
                MANTLE_SDP_DECLARATIONS,
                get(get_sdp_declarations::<RuntimeServiceId>),
            )
            .with_state(handle)
            .layer(TimeoutLayer::new(self.settings.timeout))
            .layer(RequestBodyLimitLayer::new(self.settings.max_body_size))
            .layer(ConcurrencyLimitLayer::new(
                self.settings.max_concurrent_requests,
            ))
            .layer(create_rate_limit_layer(&self.settings))
            .layer(TraceLayer::new_for_http())
            .layer(
                builder
                    .allow_headers(vec![CONTENT_TYPE, USER_AGENT])
                    .allow_methods(Any),
            );

        let listener = TcpListener::bind(&self.settings.address)
            .await
            .expect("Failed to bind address");

        let app = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, app).await
    }
}
