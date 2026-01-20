use std::{collections::HashMap, error::Error};

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LokiConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}

pub fn create_loki_layer(
    config: LokiConfig,
    handle: &Handle,
) -> Result<tracing_loki::Layer, Box<dyn Error + Send + Sync>> {
    let (loki_layer, task) = tracing_loki::layer(
        config.endpoint,
        vec![("host".into(), config.host_identifier)]
            .into_iter()
            .collect(),
        HashMap::default(),
    )?;

    handle.spawn(task);
    Ok(loki_layer)
}
