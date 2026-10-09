use std::error::Error;

use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::Resource;
use tracing::Subscriber;
use tracing_opentelemetry::MetricsLayer;
use tracing_subscriber::registry::LookupSpan;

use crate::{
    OtlpProtocol,
    metrics::{
        emit::reset_cached_instruments,
        open_metrics,
        otlp::{OtlpMetricsConfig, build_grpc_exporter, build_http_exporter},
    },
};

/// Creates the metrics layer, backed by a global meter provider with one reader
/// per enabled output.
///
/// # Parameters
///
/// - `otlp_config`: `OTLP` push endpoint; `None` disables `OTLP` export.
/// - `enable_open_metrics`: whether to enable on-demand `OpenMetrics`
///   rendering.
///
/// # Returns
///
/// - `None` if no output is enabled, so no provider is installed.
/// - `Some(layer)` if at least one output is enabled.
///
/// # Errors
///
/// If the `OTLP` exporter or the `OpenMetrics` reader fails to build.
pub fn create_metrics_layer<S>(
    otlp_config: Option<OtlpMetricsConfig>,
    enable_open_metrics: bool,
) -> Result<
    Option<MetricsLayer<S, opentelemetry_sdk::metrics::SdkMeterProvider>>,
    Box<dyn Error + Send + Sync>,
>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let open_metrics_reader = open_metrics::install(enable_open_metrics)?;

    if otlp_config.is_none() && open_metrics_reader.is_none() {
        return Ok(None);
    }

    let mut meter_provider_builder = opentelemetry_sdk::metrics::SdkMeterProvider::builder();

    if let Some(otlp_config) = otlp_config {
        let resource = Resource::builder_empty()
            .with_attributes(vec![KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                otlp_config.service.service_name.clone(),
            )])
            .build();

        let exporter = match otlp_config.service.protocol {
            OtlpProtocol::Grpc => build_grpc_exporter(otlp_config)?,
            OtlpProtocol::Http => build_http_exporter(otlp_config)?,
        };

        meter_provider_builder = meter_provider_builder
            .with_periodic_exporter(exporter)
            .with_resource(resource);
    }

    if let Some(open_metrics_reader) = open_metrics_reader {
        meter_provider_builder = meter_provider_builder.with_reader(open_metrics_reader);
    }

    let meter_provider = meter_provider_builder.build();

    global::set_meter_provider(meter_provider.clone());
    // If any instruments were created before provider initialization, drop them
    // so subsequent accesses rebuild against the configured provider.
    reset_cached_instruments();
    Ok(Some(MetricsLayer::new(meter_provider)))
}
