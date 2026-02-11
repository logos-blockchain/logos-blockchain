pub use ::tracing::Level;
use serde::{Deserialize, Serialize};
use url::Url;

pub mod console;
pub mod filter;
pub mod logger;
pub mod metrics;
pub mod tracing;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub logger: logger::Layer,
    pub tracing: tracing::Layer,
    pub filter: filter::Layer,
    pub metrics: metrics::Layer,
    pub console: console::Layer,
    #[serde(with = "serde_level")]
    pub level: Level,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MetricsLayer {
    Otlp(OtlpMetricsConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OtlpMetricsConfig {
    pub endpoint: Url,
    pub host_identifier: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConsoleLayer {
    Console(TokioConsoleConfig),
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokioConsoleConfig {
    pub bind_address: String,
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            logger: logger::Layer::Stdout,
            tracing: tracing::Layer::None,
            filter: filter::Layer::None,
            metrics: metrics::Layer::None,
            console: console::Layer::None,
            level: Level::DEBUG,
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
