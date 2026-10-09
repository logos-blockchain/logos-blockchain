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
        .without_target_info()
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

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use serial_test::serial;

    use super::{install, read_open_metrics};

    #[test]
    #[serial]
    fn read_open_metrics_is_none_when_disabled() {
        assert!(install(false).unwrap().is_none());
        assert!(read_open_metrics().unwrap().is_none());
    }

    #[test]
    #[serial]
    fn read_open_metrics_renders_recorded_counter() {
        let reader = install(true).unwrap().unwrap();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();
        provider
            .meter("test")
            .u64_counter("requests")
            .build()
            .add(3, &[]);

        let text = read_open_metrics().unwrap().unwrap();

        assert!(
            text.lines()
                .any(|line| line.starts_with("requests_total{") && line.ends_with(" 3"))
        );
        assert!(text.ends_with("# EOF\n"));
    }

    #[test]
    #[serial]
    fn install_disabled_clears_previous_registry() {
        install(true).unwrap();
        install(false).unwrap();

        assert!(read_open_metrics().unwrap().is_none());
    }
}
