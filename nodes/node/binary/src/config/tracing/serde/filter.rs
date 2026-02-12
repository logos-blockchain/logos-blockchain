use std::collections::HashMap;

use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_tracing_service::FilterLayer;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub enum Layer {
    Env(EnvConfig),
    #[default]
    None,
}

impl From<Layer> for FilterLayer {
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
    /// `HashMap` where the key is the crate/module name, and the value is the
    /// desired log level. More: <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    pub filters: HashMap<String, String>,
}
