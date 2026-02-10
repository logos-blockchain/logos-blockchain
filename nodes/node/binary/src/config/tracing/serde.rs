use lb_tracing_service::{
    ConsoleLayer, FilterLayer, LoggerLayer, MetricsLayer, TracingLayer, TracingSettings,
};
use serde::{Deserialize, Serialize};
use tracing::Level;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    pub logger: LoggerLayer,
    pub tracing: TracingLayer,
    pub filter: FilterLayer,
    pub metrics: MetricsLayer,
    pub console: ConsoleLayer,
    #[serde(with = "serde_level")]
    pub level: Level,
}

impl From<Config> for TracingSettings {
    fn from(value: Config) -> Self {
        Self {
            console: value.console,
            filter: value.filter,
            logger: value.logger,
            metrics: value.metrics,
            tracing: value.tracing,
            level: value.level,
        }
    }
}

impl From<TracingSettings> for Config {
    fn from(value: TracingSettings) -> Self {
        Self {
            console: value.console,
            filter: value.filter,
            logger: value.logger,
            metrics: value.metrics,
            tracing: value.tracing,
            level: value.level,
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
