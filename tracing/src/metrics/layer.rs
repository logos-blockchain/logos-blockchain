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
        open_metrics::{OpenMetrics, OpenMetricsConfig},
        otlp::{OtlpMetricsConfig, build_grpc_exporter, build_http_exporter},
    },
};

pub type MetricsSetup<S> = (
    MetricsLayer<S, opentelemetry_sdk::metrics::SdkMeterProvider>,
    Option<OpenMetrics>,
);

/// Creates the metrics layer, backed by a global meter provider with one reader
/// per enabled output.
///
/// # Parameters
///
/// - `otlp_config`: `OTLP` push endpoint; `None` disables `OTLP` export.
/// - `open_metrics_config`: on-demand `OpenMetrics` rendering; disabled unless
///   `enabled`.
///
/// # Returns
///
/// - `None` if no output is enabled, so no provider is installed.
/// - `Some((layer, open_metrics))` if at least one output is enabled, where
///   `open_metrics` is `Some` only if `OpenMetrics` is enabled.
///
/// # Errors
///
/// If the `OTLP` exporter or the `OpenMetrics` reader fails to build.
pub fn create_metrics_layer<S>(
    otlp_config: Option<OtlpMetricsConfig>,
    open_metrics_config: &OpenMetricsConfig,
) -> Result<Option<MetricsSetup<S>>, Box<dyn Error + Send + Sync>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    if otlp_config.is_none() && !open_metrics_config.enabled {
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

    let open_metrics = if open_metrics_config.enabled {
        let (open_metrics, reader) = OpenMetrics::new()?;
        meter_provider_builder = meter_provider_builder.with_reader(reader);
        Some(open_metrics)
    } else {
        None
    };

    let meter_provider = meter_provider_builder.build();

    global::set_meter_provider(meter_provider.clone());
    // If any instruments were created before provider initialization, drop them
    // so subsequent accesses rebuild against the configured provider.
    reset_cached_instruments();
    Ok(Some((MetricsLayer::new(meter_provider), open_metrics)))
}
