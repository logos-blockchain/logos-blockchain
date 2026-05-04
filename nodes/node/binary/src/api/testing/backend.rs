use std::fmt::{Debug, Display};

use axum::{
    Router,
    http::{
        HeaderValue,
        header::{CONTENT_TYPE, USER_AGENT},
    },
    routing::{get, post},
};
use http::StatusCode;
use lb_api_service::Backend;
use lb_http_api_common::paths::{DIAL_PEER, MANTLE_SDP_DECLARATIONS};
use lb_network_service::{NetworkService, backends::libp2p::Libp2p as NetworkBackend};
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

use crate::{
    api::{
        backend::AxumBackendSettings,
        testing::handlers::{dial_peer, get_sdp_declarations},
    },
    generic_services::{self, SdpService},
};
pub struct TestAxumBackend {
    settings: AxumBackendSettings,
}

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
        + AsServiceId<NetworkService<NetworkBackend, RuntimeServiceId>>
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
                MANTLE_SDP_DECLARATIONS,
                get(get_sdp_declarations::<RuntimeServiceId>),
            )
            .route(DIAL_PEER, post(dial_peer::<RuntimeServiceId>))
            .with_state(handle)
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
            )
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
