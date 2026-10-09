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

pub mod open_metrics {
    use std::sync::{PoisonError, RwLock};

    use opentelemetry_prometheus::PrometheusExporter;
    use opentelemetry_sdk::error::OTelSdkError;
    use prometheus::{Error, Registry, TextEncoder};

    static REGISTRY: RwLock<Option<Registry>> = RwLock::new(None);

    fn set_registry(registry: Option<Registry>) {
        // Writing assigns a complete value, so a poisoned lock should still hold valid
        // data.
        *REGISTRY.write().unwrap_or_else(PoisonError::into_inner) = registry;
    }

    pub(super) fn install(enabled: bool) -> Result<Option<PrometheusExporter>, OTelSdkError> {
        set_registry(None);
        if !enabled {
            return Ok(None);
        }
        let registry = Registry::new();
        let reader = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .build()?;
        set_registry(Some(registry));
        Ok(Some(reader))
    }

    /// Renders the current metrics as an `OpenMetrics` document.
    ///
    /// # Returns
    ///
    /// - `None` if no `OpenMetrics` registry exists (e.g. not enabled).
    /// - `Some(text)` with the rendered document, terminated by `# EOF`.
    ///
    /// # Errors
    ///
    /// If encoding the gathered metrics fails.
    pub fn read_open_metrics() -> Result<Option<String>, Error> {
        let metric_families = {
            // Writers assign a complete value, so a poisoned lock should still hold valid
            // data.
            let registry_slot = REGISTRY.read().unwrap_or_else(PoisonError::into_inner);
            let Some(registry) = registry_slot.as_ref() else {
                drop(registry_slot); // Silence early-drop false positive linter
                return Ok(None);
            };
            registry.gather()
        };

        let mut metrics_string = String::new();
        TextEncoder::new().encode_utf8(&metric_families, &mut metrics_string)?;
        metrics_string.push_str("# EOF\n");

        Ok(Some(metrics_string))
    }
}

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
