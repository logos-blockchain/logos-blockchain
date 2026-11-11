#![allow(clippy::needless_for_each, reason = "Utoipa implementation")]

use axum::{Router, routing};
use http::StatusCode;
use lb_api_service::Backend;
use overwatch::overwatch::handle::OverwatchHandle;
use tokio::net::TcpListener;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    limit::RequestBodyLimitLayer,
    timeout::TimeoutLayer,
    trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer},
};
use tracing::Level as TracingLevel;
use utoipa::OpenApi as _;
use utoipa_swagger_ui::SwaggerUi;

pub use crate::api::settings::AxumBackendSettings;
use crate::{
    RuntimeServiceId,
    api::{
        browser_access::restrict_browser_access, calls::Handle, metrics::http_metrics_middleware,
        openapi::ApiDoc, routes::api_routes,
    },
};

macro_rules! build_router {
    ($( $method:ident $path:expr => $handler:path ; )*) => {
        Router::new()$(.route($path, routing::$method($handler)))*
    };
}

pub struct AxumBackend {
    settings: AxumBackendSettings,
}

#[async_trait::async_trait]
impl Backend<RuntimeServiceId> for AxumBackend {
    type Error = std::io::Error;
    type Settings = AxumBackendSettings;

    async fn new(settings: Self::Settings) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(Self { settings })
    }

    async fn serve(self, handle: OverwatchHandle<RuntimeServiceId>) -> Result<(), Self::Error> {
        let handle = Handle::new(handle, self.settings.chain_id, self.settings.fork_digests);
        let app = api_routes!(build_router)
            .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()));

        let app = app
            .with_state(handle)
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

        #[cfg(feature = "profiling")]
        let app = {
            let pprof_routes = crate::api::pprof::create_pprof_router().layer(
                TraceLayer::new_for_http()
                    .on_request(DefaultOnRequest::new().level(TracingLevel::TRACE))
                    .on_response(DefaultOnResponse::new().level(TracingLevel::TRACE)),
            );

            app.merge(pprof_routes)
        };

        let app =
            restrict_browser_access(app, self.settings.cors_origins, self.settings.allowed_hosts);

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
