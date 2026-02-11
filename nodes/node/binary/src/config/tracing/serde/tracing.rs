use lb_tracing::tracing::otlp::OtlpTracingConfig;
use lb_tracing_service::TracingLayer;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Layer {
    Otlp(OtlpConfig),
    None,
}

impl From<Layer> for TracingLayer {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Otlp(config) => Self::Otlp(OtlpTracingConfig {
                endpoint: config.endpoint,
                sample_ratio: config.sample_ratio,
                service_name: config.service_name,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub sample_ratio: f64,
    pub service_name: String,
}
