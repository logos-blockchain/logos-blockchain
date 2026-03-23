pub use ::tracing::Level;
use serde::{Deserialize, Serialize};

pub mod console;
pub mod filter;
pub mod logger;
pub mod metrics;
pub mod tracing;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    pub logger: logger::Layers,
    pub tracing: tracing::Layer,
    pub filter: filter::Layer,
    pub metrics: metrics::Layer,
    pub console: console::Layer,
    #[serde(with = "serde_level")]
    pub level: Level,
}

const DEFAULT_LOG_LEVEL: Level = Level::DEBUG;

impl Default for Config {
    fn default() -> Self {
        Self {
            logger: logger::Layers::default(),
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
            logger: logger::Layers {
                file: None,
                loki: None,
                gelf: None,
                otlp: None,
                stdout: false,
                stderr: false,
            },
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
