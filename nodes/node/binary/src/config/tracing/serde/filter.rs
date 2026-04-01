use std::collections::HashMap;

use lb_tracing::filter::envfilter::{EnvFilterConfig, serde_filters};
use lb_tracing_service::FilterLayerSettings;
use serde::{Deserialize, Serialize};

use super::Level;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Env(EnvConfig),
    #[default]
    None,
}

impl From<Layer> for FilterLayerSettings {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Env(config) => Self::EnvFilter(EnvFilterConfig {
                filters: config.filters,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EnvConfig {
    #[serde(with = "serde_filters")]
    pub filters: HashMap<String, Level>,
}
