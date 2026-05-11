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
const DEFAULT_QUIET_TARGETS: &[(&str, Level)] = &[("libp2p_gossipsub", Level::ERROR)];
const ENVFILTER_GLOBAL_TARGET: &str = "*";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvFilterConfig {
    /// Per-target level overrides stored in typed form.
    ///
    /// The global default directive is represented internally with the `*`
    /// key and converted back into native `EnvFilter` syntax at the boundary.
    #[serde(with = "serde_validated_filters")]
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
    let mut filters = HashMap::from([(ENVFILTER_GLOBAL_TARGET.to_owned(), Level::WARN)]);
    filters.extend(
        DEFAULT_DEBUG_TARGETS
            .iter()
            .map(|target| ((*target).to_owned(), level)),
    );
    filters.extend(
        DEFAULT_QUIET_TARGETS
            .iter()
            .map(|(target, level)| ((*target).to_owned(), *level)),
    );
    filters
}

/// Validates a configured log-filter target against the known Logos target
/// catalog.
///
/// Targets outside the Logos catalog are currently accepted so that
/// external targets and not-yet-catalogued internal targets continue to work.
pub fn validate_log_filter_target(target: &str) -> Result<(), String> {
    if target == ENVFILTER_GLOBAL_TARGET {
        return Ok(());
    }

    if !lb_log_targets::is_logos_target_root(target) {
        return Ok(());
    }

    if lb_log_targets::is_valid_logos_target(target) {
        return Ok(());
    }

    Err(format!("unknown log filter target `{target}`"))
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
            if target == ENVFILTER_GLOBAL_TARGET {
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

    Ok((
        ENVFILTER_GLOBAL_TARGET.to_owned(),
        parse_filter_level(directive)?,
    ))
}

fn parse_filter_level(level: &str) -> Result<Level, String> {
    level
        .trim()
        .parse()
        .map_err(|_| format!("Invalid log filter level provided: {level}"))
}

pub mod serde_validated_filters {
    use std::collections::HashMap;

    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer, de::Error as _};
    use tracing::Level;

    use super::{parse_filter_level, validate_log_filter_target};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Level>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <HashMap<String, String>>::deserialize(deserializer)?;

        raw.into_iter()
            .map(|(target, level)| {
                validate_log_filter_target(&target).map_err(D::Error::custom)?;
                parse_filter_level(&level)
                    .map(|level| (target, level))
                    .map_err(D::Error::custom)
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

    use super::{
        ENVFILTER_GLOBAL_TARGET, EnvFilterConfig, create_envfilter_layer, default_debug_log_filter,
        parse_filter_directives, validate_log_filter_target,
    };

    #[test]
    fn create_envfilter_layer_accepts_global_and_target_directives() {
        let config = EnvFilterConfig {
            filters: HashMap::from([
                (ENVFILTER_GLOBAL_TARGET.to_owned(), Level::WARN),
                ("logos_blockchain".to_owned(), Level::DEBUG),
                ("libp2p".to_owned(), Level::INFO),
            ]),
        };

        assert!(create_envfilter_layer(&config).is_ok());
    }

    #[test]
    fn default_debug_log_filter_quiets_noisy_gossipsub_internals() {
        let filters = default_debug_log_filter(Level::DEBUG);

        assert_eq!(filters.get("libp2p_gossipsub"), Some(&Level::ERROR));
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

        assert_eq!(filters.get(ENVFILTER_GLOBAL_TARGET), Some(&Level::WARN));
        assert_eq!(filters.get("blend::service"), Some(&Level::DEBUG));
        assert_eq!(filters.get("libp2p"), Some(&Level::INFO));
    }
}
