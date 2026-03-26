use std::{collections::HashMap, error::Error};

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
    /// Per-target level overrides stored in typed form.
    ///
    /// The global default directive is represented internally with the `*`
    /// key and converted back into native `EnvFilter` syntax at the boundary.
    #[serde(with = "serde_filters")]
    pub filters: HashMap<String, Level>,
}

/// Builds the native `EnvFilter` from the typed config representation.
pub fn create_envfilter_layer(
    config: &EnvFilterConfig,
) -> Result<EnvFilter, Box<dyn Error + Send + Sync>> {
    EnvFilter::try_new(envfilter_directives(&config.filters)).map_err(Into::into)
}

#[must_use]
/// Returns the built-in verbose filter policy for `DEBUG` and `TRACE`.
pub fn default_envfilter_config(level: Level) -> Option<EnvFilterConfig> {
    (level >= Level::DEBUG).then(|| EnvFilterConfig {
        filters: default_debug_log_filter(level),
    })
}

#[must_use]
/// Builds the default verbose filter policy as a typed map.
pub fn default_debug_log_filter(level: Level) -> HashMap<String, Level> {
    let mut filters = HashMap::from([("*".to_owned(), Level::WARN)]);
    filters.extend(
        DEFAULT_DEBUG_TARGETS
            .iter()
            .map(|target| ((*target).to_owned(), level)),
    );
    filters
}

/// Converts the typed filter config into native `EnvFilter` directives.
fn envfilter_directives(filters: &HashMap<String, Level>) -> String {
    let mut directives = filters
        .iter()
        .map(|(target, level)| {
            if target == "*" {
                level.as_str().to_owned()
            } else {
                format!("{target}={}", level.as_str())
            }
        })
        .collect::<Vec<_>>();

    directives.sort();
    directives.join(",")
}

pub mod serde_filters {
    use std::collections::HashMap;

    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de::Error as _};
    use tracing::Level;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Level>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <HashMap<String, String>>::deserialize(deserializer)?;

        raw.into_iter()
            .map(|(target, level)| {
                level
                    .parse()
                    .map(|level| (target, level))
                    .map_err(|e| D::Error::custom(format!("invalid log level {e}")))
            })
            .collect()
    }

    pub fn serialize<S, H>(
        value: &HashMap<String, Level, H>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        H: std::hash::BuildHasher,
    {
        value
            .iter()
            .map(|(target, level)| (target.clone(), level.as_str().to_owned()))
            .collect::<HashMap<_, _>>()
            .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tracing::Level;

    use super::{EnvFilterConfig, create_envfilter_layer};

    #[test]
    fn create_envfilter_layer_accepts_global_and_target_directives() {
        let config = EnvFilterConfig {
            filters: HashMap::from([
                ("*".to_owned(), Level::WARN),
                ("logos_blockchain".to_owned(), Level::DEBUG),
                ("libp2p".to_owned(), Level::INFO),
            ]),
        };

        assert!(create_envfilter_layer(&config).is_ok());
    }
}
