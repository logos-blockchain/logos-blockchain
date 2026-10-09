use lb_tracing::{OtlpProtocol, OtlpServiceConfig, metrics::otlp::OtlpMetricsConfig};
use lb_tracing_service::MetricsLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    otlp: Option<OtlpConfig>,
    enable_open_metrics: bool,
}

impl Layer {
    #[must_use]
    pub const fn from_otlp(otlp: Option<OtlpConfig>) -> Self {
        Self {
            otlp,
            enable_open_metrics: false,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self {
            otlp: None,
            enable_open_metrics: false,
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
        Self {
            otlp,
            enable_open_metrics: value.enable_open_metrics,
        }
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
