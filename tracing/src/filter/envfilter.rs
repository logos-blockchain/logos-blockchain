use std::{collections::HashMap, error::Error};

use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvFilterConfig {
    /// `HashMap` where the key is the crate/module name, and the value is the
    /// desired log level. More: <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    pub filters: HashMap<String, String>,
}

pub fn create_envfilter_layer(
    config: EnvFilterConfig,
) -> Result<EnvFilter, Box<dyn Error + Send + Sync>> {
    let filter_string = config
        .filters
        .into_iter()
        .map(|(target, level)| format!("{target}={level}"))
        .collect::<Vec<_>>()
        .join(",");

    EnvFilter::try_new(filter_string).map_err(Into::into)
}
