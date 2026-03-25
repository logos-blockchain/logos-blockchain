use std::error::Error;

use serde::{Deserialize, Serialize};
use tracing::Level;
use tracing_subscriber::EnvFilter;

const DEFAULT_DEBUG_TARGETS: &[&str] = &[
    "logos_blockchain",
    "blend",
    "chain",
    "chain_network",
    "chain_leader",
    "cryptarchia",
    "ledger",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvFilterConfig {
    /// `EnvFilter` directive string. More:
    /// <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    pub filter: String,
}

pub fn create_envfilter_layer(
    config: EnvFilterConfig,
) -> Result<EnvFilter, Box<dyn Error + Send + Sync>> {
    EnvFilter::try_new(config.filter).map_err(Into::into)
}

#[must_use]
pub fn default_envfilter_config(level: Level) -> Option<EnvFilterConfig> {
    (level >= Level::DEBUG).then(|| EnvFilterConfig {
        filter: default_debug_log_filter(level),
    })
}

#[must_use]
pub fn default_debug_log_filter(level: Level) -> String {
    let mut directives = vec!["warn".to_owned()];
    let app_level = default_filter_level(level);

    directives.extend(
        DEFAULT_DEBUG_TARGETS
            .iter()
            .map(|target| format!("{target}={app_level}")),
    );

    directives.join(",")
}

const fn default_filter_level(level: Level) -> &'static str {
    match level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}
