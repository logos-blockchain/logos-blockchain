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

/// Validates a configured log-filter target against the known Logos target
/// catalog.
///
/// Targets outside the Logos catalog are currently accepted so that
/// external targets and not-yet-catalogued internal targets continue to work.
pub fn validate_log_filter_target(target: &str) -> Result<(), String> {
    if target == "*" {
        return Ok(());
    }

    if lb_log_targets::is_logos_target_root(target)
        && !lb_log_targets::is_valid_logos_target_prefix(target)
    {
        return Err(format!("unknown log filter target `{target}`"));
    }

    Ok(())
}

/// Parses comma-separated filter directives into the typed filter config form.
///
/// Supported syntax:
/// - `target=level`
/// - bare global level such as `warn`
pub fn parse_filter_directives(raw: &str) -> Result<HashMap<String, Level>, String> {
    let filters = raw
        .split(',')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(parse_filter_directive)
        .collect::<Result<HashMap<_, _>, _>>()?;

    if filters.is_empty() {
        return Err(format!("Invalid log filter provided: {raw}"));
    }

    Ok(filters)
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

fn parse_filter_directive(directive: &str) -> Result<(String, Level), String> {
    if let Some((target, level)) = directive.split_once('=') {
        let target = target.trim();
        let level = level.trim();

        if target.is_empty() || level.is_empty() {
            return Err(format!("Invalid log filter directive: {directive}"));
        }

        validate_log_filter_target(target)?;
        return Ok((target.to_owned(), parse_filter_level(level)?));
    }

    Ok(("*".to_owned(), parse_filter_level(directive)?))
}

fn parse_filter_level(level: &str) -> Result<Level, String> {
    level
        .trim()
        .parse()
        .map_err(|_| format!("Invalid log filter level provided: {level}"))
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

pub mod serde_validated_filters {
    use std::collections::HashMap;

    use serde::{Deserializer, Serializer, de::Error as _};
    use tracing::Level;

    use super::{serde_filters, validate_log_filter_target};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Level>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let filters = serde_filters::deserialize(deserializer)?;
        for target in filters.keys() {
            validate_log_filter_target(target).map_err(D::Error::custom)?;
        }
        Ok(filters)
    }

    pub fn serialize<S, H>(
        value: &HashMap<String, Level, H>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        H: std::hash::BuildHasher,
    {
        serde_filters::serialize(value, serializer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tracing::Level;

    use super::{
        EnvFilterConfig, create_envfilter_layer, parse_filter_directives,
        validate_log_filter_target,
    };

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

    #[test]
    fn validate_log_filter_target_rejects_unknown_blend_target() {
        let error = validate_log_filter_target("blend::service::missing")
            .expect_err("unknown blend target should fail");

        assert_eq!(error, "unknown log filter target `blend::service::missing`");
    }

    #[test]
    fn validate_log_filter_target_accepts_external_targets() {
        assert!(validate_log_filter_target("libp2p").is_ok());
    }

    #[test]
    fn parse_filter_directives_accepts_global_and_target_directives() {
        let filters = parse_filter_directives("warn,blend::service=debug,libp2p=info")
            .expect("filter directives should parse");

        assert_eq!(filters.get("*"), Some(&Level::WARN));
        assert_eq!(filters.get("blend::service"), Some(&Level::DEBUG));
        assert_eq!(filters.get("libp2p"), Some(&Level::INFO));
    }
}
