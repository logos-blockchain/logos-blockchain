use std::{collections::HashMap, error::Error};

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::Resource;
use serde::{Deserialize, Serialize};
use tonic::metadata::MetadataMap;
use tracing::Subscriber;
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::registry::LookupSpan;

use crate::{OtlpProtocol, OtlpServiceConfig, metrics::emit::reset_cached_instruments};

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
            config.service.service_name.clone(),
        )])
        .build();

    let exporter = match config.service.protocol {
        OtlpProtocol::Grpc => build_grpc_exporter(config)?,
        OtlpProtocol::Http => build_http_exporter(config)?,
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

fn build_grpc_exporter(
    config: OtlpMetricsConfig,
) -> Result<opentelemetry_otlp::MetricExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        let mut metadata = MetadataMap::new();
        metadata.insert("authorization", auth.parse()?);
        builder = builder.with_metadata(metadata);
    }

    Ok(builder.build()?)
}

fn build_http_exporter(
    config: OtlpMetricsConfig,
) -> Result<opentelemetry_otlp::MetricExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        builder = builder.with_headers(HashMap::from([("authorization".to_owned(), auth)]));
    }

    Ok(builder.build()?)
}
