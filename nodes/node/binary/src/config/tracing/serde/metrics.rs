use lb_tracing::{OtlpProtocol, OtlpServiceConfig, metrics::otlp::OtlpMetricsConfig};
use lb_tracing_service::MetricsLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Otlp(OtlpConfig),
    #[default]
    None,
}

impl From<Layer> for MetricsLayerSettings {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Otlp(config) => Self::Otlp(OtlpMetricsConfig {
                service: OtlpServiceConfig {
                    url: config.endpoint,
                    service_name: config.service_name,
                    authorization_header: config.authorization_header,
                    protocol: config.protocol,
                },
            }),
            Layer::None => Self::None,
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
