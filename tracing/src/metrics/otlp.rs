use std::error::Error;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::Resource;
use serde::{Deserialize, Serialize};
use tonic::metadata::MetadataMap;
use tracing::Subscriber;
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::registry::LookupSpan;

use crate::{OtlpServiceConfig, metrics::emit::reset_cached_instruments};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpMetricsConfig {
    #[serde(flatten)]
    pub service: OtlpServiceConfig,
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
            config.service.service_name,
        )])
        .build();

    let exporter = {
        let mut exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(config.service.url.to_string());
        if let Some(auth_header) = config.service.authorization_header {
            let mut metadata = MetadataMap::new();
            metadata.insert("authorization", auth_header.parse()?);
            exporter = exporter.with_metadata(metadata);
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
