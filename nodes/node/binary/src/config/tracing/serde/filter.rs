use lb_tracing::filter::envfilter::EnvFilterConfig;
use lb_tracing_service::FilterLayerSettings;
use serde::{Deserialize, Serialize};

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
                filter: config.filter,
            }),
            Layer::None => Self::None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EnvConfig {
    /// `EnvFilter` directive string. More:
    /// <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    pub filter: String,
}
