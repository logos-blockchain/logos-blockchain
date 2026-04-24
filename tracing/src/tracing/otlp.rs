use std::error::Error;

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
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
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpTracingConfig {
    pub endpoint: Url,
    pub sample_ratio: f64,
    pub service_name: String,
    pub authorization_header: Option<String>,
}

pub fn create_otlp_tracing_layer<S>(
    config: OtlpTracingConfig,
) -> Result<OpenTelemetryLayer<S, Tracer>, Box<dyn Error + Send + Sync>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let resource = Resource::builder()
        .with_attributes(vec![KeyValue::new(SERVICE_NAME, config.service_name)])
        .build();

    let exporter = {
        let mut exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.to_string());
        if let Some(auth_header) = config.authorization_header {
            let mut metadata = MetadataMap::new();
            metadata.insert("authorization", auth_header.parse()?);
            exporter = exporter.with_metadata(metadata);
        }

        exporter.build()?
    };

    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            config.sample_ratio,
        ))))
        .with_batch_exporter(exporter)
        .build();

    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(tracer_provider.clone());

    let tracer: Tracer = tracer_provider.tracer("LogosBlockchainTracer");

    Ok(OpenTelemetryLayer::new(tracer))
}
