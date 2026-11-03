use lb_tracing::{OtlpProtocol, OtlpServiceConfig, tracing::otlp::OtlpTracingConfig};
use lb_tracing_service::TracingLayerSettings;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Otlp(OtlpConfig),
    #[default]
    None,
}

impl From<Layer> for TracingLayerSettings {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Otlp(config) => Self::Otlp(OtlpTracingConfig {
                service: OtlpServiceConfig {
                    url: config.endpoint,
                    service_name: config.service_name,
                    authorization_header: config.authorization_header,
                    protocol: config.protocol,
                },
                sample_ratio: config.sample_ratio,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub sample_ratio: f64,
    pub service_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_header: Option<String>,
    pub protocol: OtlpProtocol,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: super::default_otlp_endpoint(),
            sample_ratio: 0.5,
            service_name: super::default_service_name(),
            authorization_header: None,
            protocol: OtlpProtocol::default(),
        }
    }
}
