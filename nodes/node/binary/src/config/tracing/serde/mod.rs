#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "Serde conditional serialization skip requires a specific function signature."
)]

pub use ::tracing::Level;
use serde::{Deserialize, Serialize};

use crate::config::utils;

pub mod console;
pub mod filter;
pub mod logger;
pub mod metrics;
pub mod tracing;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    #[serde(skip_serializing_if = "utils::is_default")]
    pub logger: logger::Layer,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub tracing: tracing::Layer,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub filter: filter::Layer,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub metrics: metrics::Layer,
    #[serde(skip_serializing_if = "utils::is_default")]
    pub console: console::Layer,
    #[serde(with = "serde_level")]
    #[serde(skip_serializing_if = "is_default_log_level")]
    pub level: Level,
}

const DEFAULT_LOG_LEVEL: Level = Level::DEBUG;

fn is_default_log_level(level: &Level) -> bool {
    *level == DEFAULT_LOG_LEVEL
}

impl Default for Config {
    fn default() -> Self {
        Self {
            logger: logger::Layer::default(),
            tracing: tracing::Layer::default(),
            filter: filter::Layer::default(),
            metrics: metrics::Layer::default(),
            console: console::Layer::default(),
            level: DEFAULT_LOG_LEVEL,
        }
    }
}

impl Config {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            logger: logger::Layer::None,
            tracing: tracing::Layer::None,
            filter: filter::Layer::None,
            metrics: metrics::Layer::None,
            console: console::Layer::None,
            level: DEFAULT_LOG_LEVEL,
        }
    }
}

mod serde_level {
    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de::Error as _};

    use super::Level;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Level, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = <String>::deserialize(deserializer)?;
        v.parse()
            .map_err(|e| D::Error::custom(format!("invalid log level {e}")))
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "Signature must match serde requirement."
    )]
    pub fn serialize<S>(value: &Level, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.as_str().serialize(serializer)
    }
}
