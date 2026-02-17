#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

use core::cmp::Ordering;

use lb_tracing::tracing::otlp::OtlpTracingConfig;
use lb_tracing_service::TracingLayer;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub enum Layer {
    Otlp(OtlpConfig),
    #[default]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OtlpConfig {
    pub endpoint: Url,
    pub service_name: String,

    #[serde(default = "default_sample_ratio")]
    #[serde(skip_serializing_if = "is_default_sample_ratio")]
    pub sample_ratio: f64,
}

const fn default_sample_ratio() -> f64 {
    0.5
}

fn is_default_sample_ratio(sample_ratio: &f64) -> bool {
    matches!(
        sample_ratio.partial_cmp(&default_sample_ratio()),
        Some(Ordering::Equal)
    )
}
