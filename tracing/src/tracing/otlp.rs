use std::{collections::HashMap, error::Error};

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::{WithExportConfig as _, WithHttpConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::{
    Resource,
    propagation::TraceContextPropagator,
    trace::{Sampler, SdkTracerProvider, Tracer},
};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use serde::{Deserialize, Serialize};
use tonic::metadata::MetadataMap;
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::registry::LookupSpan;

use crate::{OtlpProtocol, OtlpServiceConfig};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpTracingConfig {
    #[serde(flatten)]
    pub service: OtlpServiceConfig,
    pub sample_ratio: f64,
}

pub fn create_otlp_tracing_layer<S>(
    config: OtlpTracingConfig,
) -> Result<OpenTelemetryLayer<S, Tracer>, Box<dyn Error + Send + Sync>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let resource = Resource::builder()
        .with_attributes(vec![KeyValue::new(
            SERVICE_NAME,
            config.service.service_name.clone(),
        )])
        .build();

    let sample_ratio = config.sample_ratio;

    let exporter = match config.service.protocol {
        OtlpProtocol::Grpc => build_grpc_exporter(config)?,
        OtlpProtocol::Http => build_http_exporter(config)?,
    };

    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            sample_ratio,
        ))))
        .with_batch_exporter(exporter)
        .build();

    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(tracer_provider.clone());

    let tracer: Tracer = tracer_provider.tracer("LogosBlockchainTracer");

    Ok(OpenTelemetryLayer::new(tracer))
}

fn build_grpc_exporter(
    config: OtlpTracingConfig,
) -> Result<opentelemetry_otlp::SpanExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
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
    config: OtlpTracingConfig,
) -> Result<opentelemetry_otlp::SpanExporter, Box<dyn Error + Send + Sync>> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(config.service.url.to_string());

    if let Some(auth) = config.service.authorization_header {
        builder = builder.with_headers(HashMap::from([("authorization".to_owned(), auth)]));
    }

    Ok(builder.build()?)
}
