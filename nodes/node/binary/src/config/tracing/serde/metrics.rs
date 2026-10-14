use lb_tracing::{
    OtlpProtocol, OtlpServiceConfig,
    metrics::{open_metrics, otlp::OtlpMetricsConfig},
};
use lb_tracing_service::MetricsLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    otlp: Option<OtlpConfig>,
    open_metrics: Option<OpenMetricsConfig>,
}

impl Layer {
    #[must_use]
    pub const fn from_otlp(otlp: Option<OtlpConfig>) -> Self {
        Self {
            otlp,
            open_metrics: None,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self {
            otlp: None,
            open_metrics: None,
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
                .is_some_and(|open_metrics_config| open_metrics_config.enabled),
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
