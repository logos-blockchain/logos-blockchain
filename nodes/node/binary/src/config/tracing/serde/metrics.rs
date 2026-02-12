use lb_tracing::metrics::otlp::OtlpMetricsConfig;
use lb_tracing_service::MetricsLayer;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Otlp(OtlpConfig),
    #[default]
    None,
}

impl From<Layer> for MetricsLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Otlp(config) => Self::Otlp(OtlpMetricsConfig {
                endpoint: config.endpoint,
                host_identifier: config.host_identifier,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}
