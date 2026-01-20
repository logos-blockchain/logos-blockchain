use std::error::Error;

use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{
    Resource,
    propagation::TraceContextPropagator,
    trace::{BatchConfig, Sampler, Tracer},
};
use opentelemetry_semantic_conventions::resource::SERVICE_NAME;
use serde::{Deserialize, Serialize};
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::registry::LookupSpan;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpTracingConfig {
    pub endpoint: Url,
    pub sample_ratio: f64,
    pub service_name: String,
}

pub fn create_otlp_tracing_layer<S>(
    config: OtlpTracingConfig,
) -> Result<OpenTelemetryLayer<S, Tracer>, Box<dyn Error + Send + Sync>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let resource = Resource::new(vec![KeyValue::new(SERVICE_NAME, config.service_name)]);
    let tracer_provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_trace_config(
            opentelemetry_sdk::trace::Config::default()
                .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                    config.sample_ratio,
                ))))
                .with_resource(resource),
        )
        .with_batch_config(BatchConfig::default())
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(config.endpoint),
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)?;

    global::set_text_map_propagator(TraceContextPropagator::new());
    global::set_tracer_provider(tracer_provider.clone());
    let tracer: Tracer = tracer_provider.tracer("LogosBlockchainTracer");

    Ok(OpenTelemetryLayer::new(tracer))
}
