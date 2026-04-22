use std::error::Error;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::Resource;
use serde::{Deserialize, Serialize};
use tonic::{
    Request, Status,
    metadata::{Ascii, MetadataValue},
};
use tracing::Subscriber;
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::registry::LookupSpan;
use url::Url;

use crate::metrics::emit::reset_cached_instruments;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpMetricsConfig {
    pub endpoint: Url,
    pub host_identifier: String,
    pub authorization_header: Option<String>,
}

pub fn create_otlp_metrics_layer<S>(
    config: OtlpMetricsConfig,
) -> Result<
    MetricsLayer<S, opentelemetry_sdk::metrics::SdkMeterProvider>,
    Box<dyn Error + Send + Sync>,
>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let resource = Resource::builder_empty()
        .with_attributes(vec![KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
            config.host_identifier,
        )])
        .build();

    let exporter = {
        let mut exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.to_string());
        if let Some(auth_header) = config.authorization_header {
            let Ok(auth_header_metadata) = auth_header.parse::<MetadataValue<Ascii>>() else {
                return Err(Box::new(Status::invalid_argument(
                    "Invalid authorization header value",
                )));
            };
            exporter = exporter.with_interceptor(move |mut req: Request<()>| {
                req.metadata_mut()
                    .insert("authorization", auth_header_metadata.clone());
                Ok(req)
            });
        }

        exporter.build()?
    };

    let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_periodic_exporter(exporter)
        .with_resource(resource)
        .build();

    global::set_meter_provider(meter_provider.clone());
    // If any instruments were created before provider initialization, drop them
    // so subsequent accesses rebuild against the configured provider.
    reset_cached_instruments();
    Ok(MetricsLayer::new(meter_provider))
}
