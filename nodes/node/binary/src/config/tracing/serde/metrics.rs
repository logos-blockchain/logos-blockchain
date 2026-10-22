use lb_tracing::{
    OtlpProtocol, OtlpServiceConfig,
    metrics::{open_metrics, otlp::OtlpMetricsConfig},
};
use lb_tracing_service::MetricsLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    otlp: Option<OtlpConfig>,
    open_metrics: Option<OpenMetricsConfig>,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            otlp: None,
            open_metrics: Some(OpenMetricsConfig::default()),
        }
    }
}

impl Layer {
    #[must_use]
    pub const fn new(otlp: Option<OtlpConfig>, open_metrics: Option<OpenMetricsConfig>) -> Self {
        Self { otlp, open_metrics }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self {
            otlp: None,
            open_metrics: Some(OpenMetricsConfig::disabled()),
        }
    }
}

impl From<Layer> for MetricsLayerSettings {
    fn from(value: Layer) -> Self {
        let otlp = value.otlp.map(|otlp| OtlpMetricsConfig {
            service: OtlpServiceConfig {
                url: otlp.endpoint,
                service_name: otlp.service_name,
                authorization_header: otlp.authorization_header,
                protocol: otlp.protocol,
            },
        });
        let open_metrics = open_metrics::OpenMetricsConfig {
            enabled: value
                .open_metrics
                .is_some_and(|open_metrics| open_metrics.enabled),
        };

        Self { otlp, open_metrics }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub service_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_header: Option<String>,
    pub protocol: OtlpProtocol,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenMetricsConfig {
    /// Explicit flag, since an empty block is ambiguous as an on switch.
    /// Can be removed if and when more attributes are added.
    pub enabled: bool,
}

impl Default for OpenMetricsConfig {
    fn default() -> Self {
        Self::enabled()
    }
}

impl OpenMetricsConfig {
    #[must_use]
    pub const fn enabled() -> Self {
        Self { enabled: true }
    }

    #[must_use]
    pub const fn disabled() -> Self {
        Self { enabled: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_metrics_follows_explicit_flag() {
        let layer: Layer = serde_yaml::from_str("open_metrics:\n  enabled: false").unwrap();
        assert!(!MetricsLayerSettings::from(layer).open_metrics.enabled);
        let layer: Layer = serde_yaml::from_str("open_metrics:\n  enabled: true").unwrap();
        assert!(MetricsLayerSettings::from(layer).open_metrics.enabled);
    }

    #[test]
    fn open_metrics_is_enabled_when_block_is_absent() {
        let layer: Layer = serde_yaml::from_str("otlp: null").unwrap();
        assert!(MetricsLayerSettings::from(layer).open_metrics.enabled);
    }

    #[test]
    fn open_metrics_is_disabled_when_block_is_null() {
        let layer: Layer = serde_yaml::from_str("open_metrics: null").unwrap();
        assert!(!MetricsLayerSettings::from(layer).open_metrics.enabled);
    }
}
