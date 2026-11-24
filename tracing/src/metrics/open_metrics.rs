use opentelemetry_prometheus::PrometheusExporter;
use opentelemetry_sdk::error::OTelSdkError;
use prometheus::{Error, Registry, TextEncoder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenMetricsConfig {
    pub enabled: bool,
}

impl OpenMetricsConfig {
    #[must_use]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }
}

/// On-demand `OpenMetrics` source, backed by the registry its paired reader
/// feeds.
pub struct OpenMetrics {
    registry: Registry,
}

impl OpenMetrics {
    /// Creates the registry and the reader that feeds it.
    ///
    /// # Errors
    ///
    /// If the reader fails to build.
    pub fn new() -> Result<(Self, PrometheusExporter), OTelSdkError> {
        let registry = Registry::new();
        let reader = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_target_info()
            .build()?;

        Ok((Self { registry }, reader))
    }

    /// Renders the current metrics as an `OpenMetrics` document.
    ///
    /// # Errors
    ///
    /// If encoding the gathered metrics fails.
    pub fn read(&self) -> Result<String, Error> {
        let metric_families = self.registry.gather();

        let mut metrics_string = String::new();
        TextEncoder::new().encode_utf8(&metric_families, &mut metrics_string)?;
        metrics_string.push_str("# EOF\n");

        Ok(metrics_string)
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::SdkMeterProvider;

    use super::OpenMetrics;

    #[test]
    fn read_renders_recorded_counter() {
        let (open_metrics, reader) = OpenMetrics::new().unwrap();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        provider
            .meter("test")
            .u64_counter("requests")
            .build()
            .add(3, &[]);

        let text = open_metrics.read().unwrap();

        assert!(
            text.lines()
                .any(|line| line.starts_with("requests_total{") && line.ends_with(" 3"))
        );
        assert!(text.ends_with("# EOF\n"));
    }
}
