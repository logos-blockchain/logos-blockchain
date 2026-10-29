//! Upgrade of a 0.3.2 user config to the current format.
//!
//! 0.3.3 renamed a few keys and moved every duration to humantime strings
//! (`5s`, `250ms`), so a 0.3.2 config no longer parses. [`migrate_0_3_2`]
//! rewrites the parsed YAML in place, and [`UserConfig::load`] uses it to
//! upgrade an old config on startup.
//!
//! [`UserConfig::load`]: crate::config::UserConfig::load

use core::time::Duration;
use std::collections::HashMap;

use lb_tracing::filter::envfilter::default_envfilter_config;
use serde_yaml::{Mapping, Value};
use tracing::Level;

use crate::config::TracingConfig;

#[derive(Clone, Copy)]
enum Unit {
    Seconds,
    Millis,
}

impl Unit {
    const fn duration(self, value: u64) -> Duration {
        match self {
            Self::Seconds => Duration::from_secs(value),
            Self::Millis => Duration::from_millis(value),
        }
    }
}

/// Durations 0.3.2 stored as a bare integer in the given unit.
const INTEGER_DURATIONS: &[(&[&str], &str, Unit)] = &[
    (&["api", "backend"], "timeout", Unit::Seconds),
    (
        &["network", "backend", "swarm", "chain_sync"],
        "peer_response_timeout",
        Unit::Millis,
    ),
    // Written by the 0.3.3 release branch before this upgrade landed.
    (
        &["network", "backend", "swarm", "chain_sync"],
        "inbound_download_stream_timeout",
        Unit::Millis,
    ),
];

/// Durations 0.3.2 stored as a bare integer under a unit-suffixed key.
const RENAMED_DURATIONS: &[(&[&str], &str, &str, Unit)] = &[
    (
        &["network", "backend", "swarm", "kademlia"],
        "query_timeout_secs",
        "query_timeout",
        Unit::Seconds,
    ),
    (
        &["network", "backend", "swarm", "kademlia"],
        "periodic_bootstrap_interval_secs",
        "periodic_bootstrap_interval",
        Unit::Seconds,
    ),
    (
        &["network", "backend", "swarm", "identify"],
        "interval_secs",
        "interval",
        Unit::Seconds,
    ),
    (
        &["network", "backend", "swarm", "nat", "autonat"],
        "probe_interval_millisecs",
        "probe_interval",
        Unit::Millis,
    ),
];

/// Rewrites a 0.3.2 user config into the current format and returns whether
/// anything had to change. A config already in the current format is left
/// untouched.
pub fn migrate_0_3_2(config: &mut Value) -> bool {
    let mut changed = humanize_durations(config);

    for (path, key, unit) in INTEGER_DURATIONS {
        if let Some(mapping) = mapping_at(config, path)
            && let Some(Value::Number(number)) = mapping.get(*key)
            && let Some(value) = number.as_u64()
        {
            mapping.insert((*key).into(), humantime(unit.duration(value)));
            changed = true;
        }
    }

    for (path, old_key, new_key, unit) in RENAMED_DURATIONS {
        if let Some(mapping) = mapping_at(config, path)
            && let Some(value) = mapping.remove(*old_key)
        {
            let value = match value {
                Value::Number(number) => number.as_u64().map_or(Value::Number(number), |value| {
                    humantime(unit.duration(value))
                }),
                other => other,
            };
            mapping.insert((*new_key).into(), value);
            changed = true;
        }
    }

    changed |= migrate_kademlia_caching(config);
    changed |= migrate_open_metrics(config);
    // Only alongside the other changes: a current config may well list the
    // built-in filter.
    if changed {
        drop_default_log_filter(config);
    }
    changed
}

/// 0.3.2 wrote the built-in log filter for the configured level into every
/// config. Unchanged, it's dropped, so the node keeps following the built-in
/// filter, which is applied at startup when none is configured.
fn drop_default_log_filter(config: &mut Value) {
    let Some(tracing) = mapping_at(config, &["tracing"]) else {
        return;
    };
    let level = match tracing.get("level") {
        None => TracingConfig::default().level,
        Some(level) => match level.as_str().map(str::parse::<Level>) {
            Some(Ok(level)) => level,
            _ => return,
        },
    };
    let Some(Value::Tagged(filter)) = tracing.get("filter") else {
        return;
    };
    let Some(filters) = filter.value.get("filters").and_then(Value::as_mapping) else {
        return;
    };
    let Some(built_in) = default_envfilter_config(level) else {
        return;
    };
    let configured: Option<HashMap<&str, String>> = filters
        .iter()
        .map(|(target, level)| Some((target.as_str()?, level.as_str()?.to_uppercase())))
        .collect();
    let built_in: HashMap<&str, String> = built_in
        .filters
        .iter()
        .map(|(target, level)| (target.as_str(), level.to_string()))
        .collect();
    if filter.tag == "Env" && configured == Some(built_in) {
        tracing.remove("filter");
    }
}

/// 0.3.2 `caching: null | {type: Disabled} | {type: Enabled, config:
/// {max_peers}}` became `cache_size: null | <max_peers>`. An unset `caching`
/// meant libp2p's default, one cached peer, which is the new default.
fn migrate_kademlia_caching(config: &mut Value) -> bool {
    let Some(kademlia) = mapping_at(config, &["network", "backend", "swarm", "kademlia"]) else {
        return false;
    };
    let Some(caching) = kademlia.remove("caching") else {
        return false;
    };
    match caching.get("type").and_then(Value::as_str) {
        Some("Disabled") => {
            kademlia.insert("cache_size".into(), Value::Null);
        }
        Some("Enabled") => {
            let max_peers = caching
                .get("config")
                .and_then(|config| config.get("max_peers"))
                .cloned()
                .unwrap_or(Value::Null);
            kademlia.insert("cache_size".into(), max_peers);
        }
        _ => {}
    }
    true
}

/// 0.3.2 `enable_open_metrics: <bool>` is dropped rather than translated to
/// `open_metrics: {enabled: <bool>}`: 0.3.2 wrote its `false` default into
/// every config, and upgraded nodes are meant to get the new always-on default.
fn migrate_open_metrics(config: &mut Value) -> bool {
    mapping_at(config, &["tracing", "metrics"])
        .and_then(|metrics| metrics.remove("enable_open_metrics"))
        .is_some()
}

/// Rewrites the two 0.3.2 duration encodings found anywhere in the config:
/// `{secs, nanos}` mappings and `"<secs>.<9 digit nanos>"` strings.
fn humanize_durations(value: &mut Value) -> bool {
    if let Some(duration) = old_duration(value) {
        *value = humantime(duration);
        return true;
    }
    match value {
        Value::Mapping(mapping) => mapping
            .values_mut()
            .fold(false, |changed, value| humanize_durations(value) | changed),
        Value::Sequence(values) => values
            .iter_mut()
            .fold(false, |changed, value| humanize_durations(value) | changed),
        Value::Tagged(tagged) => humanize_durations(&mut tagged.value),
        _ => false,
    }
}

fn old_duration(value: &Value) -> Option<Duration> {
    match value {
        Value::Mapping(mapping) if mapping.len() == 2 => {
            let secs = mapping.get("secs")?.as_u64()?;
            let nanos = u32::try_from(mapping.get("nanos")?.as_u64()?).ok()?;
            Some(Duration::new(secs, nanos))
        }
        Value::String(string) => {
            let (secs, nanos) = string.split_once('.')?;
            if secs.is_empty()
                || nanos.len() != 9
                || !secs
                    .bytes()
                    .chain(nanos.bytes())
                    .all(|b| b.is_ascii_digit())
            {
                return None;
            }
            Some(Duration::new(secs.parse().ok()?, nanos.parse().ok()?))
        }
        _ => None,
    }
}

fn humantime(duration: Duration) -> Value {
    Value::String(humantime::format_duration(duration).to_string())
}

fn mapping_at<'a>(config: &'a mut Value, path: &[&str]) -> Option<&'a mut Mapping> {
    path.iter()
        .try_fold(config, |value, key| value.get_mut(*key))?
        .as_mapping_mut()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::UserConfig;

    /// A config written by `init-config` on the 0.3.3 release branch before
    /// this upgrade, i.e. in the 0.3.2 format.
    const CONFIG_0_3_2: &str = include_str!("fixtures/user_config_0_3_2.yaml");
    /// The keystore `init-config` wrote next to [`CONFIG_0_3_2`].
    const KEYSTORE_0_3_2: &str = include_str!("fixtures/keystore_0_3_2.yaml");

    fn migrated(yaml: &str) -> (Value, bool) {
        let mut config = serde_yaml::from_str(yaml).unwrap();
        let changed = migrate_0_3_2(&mut config);
        (config, changed)
    }

    fn yaml(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn load_upgrades_a_0_3_2_config_and_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_config.yaml");
        std::fs::write(&path, CONFIG_0_3_2).unwrap();

        let config = UserConfig::load(&path).unwrap();
        assert_eq!(config.api.backend.timeout, Duration::from_secs(30));
        assert_eq!(
            config
                .network
                .backend
                .swarm
                .chain_sync
                .peer_response_timeout,
            Duration::from_secs(5)
        );
        assert_eq!(config.network.backend.swarm.kademlia.cache_size, Some(1));
        assert_eq!(
            config.mempool.tx_ttl,
            Some(Duration::from_hours(9)),
            "string durations keep their value"
        );

        let backup = dir.path().join("user_config.yaml.0.3.2.bak");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), CONFIG_0_3_2);

        let (_, changed) = migrated(&std::fs::read_to_string(&path).unwrap());
        assert!(!changed, "the rewritten config is in the current format");
        UserConfig::load(&path).unwrap();
        assert!(!dir.path().join("user_config.yaml.0.3.2.bak.1").exists());
    }

    #[test]
    fn load_writes_only_the_non_default_values_when_there_is_a_keystore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_config.yaml");
        std::fs::write(&path, CONFIG_0_3_2).unwrap();
        std::fs::write(dir.path().join("keystore.yaml"), KEYSTORE_0_3_2).unwrap();

        let config = UserConfig::load(&path).unwrap();

        // The keys and every default are left to the keystore and the
        // defaults; what the 0.3.2 init set explicitly is kept.
        let written: Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            written["network"]["backend"]["swarm"]
                .get("node_key")
                .is_none()
        );
        assert!(written.get("kms").is_none());
        assert!(written.get("mempool").is_none());
        assert!(written["tracing"].get("metrics").is_none());
        assert!(
            written["tracing"].get("filter").is_none(),
            "the built-in log filter is left to the node"
        );
        assert_eq!(
            serde_yaml::to_value(&config).unwrap()["tracing"]["metrics"]["open_metrics"]["enabled"],
            Value::Bool(true),
            "OpenMetrics takes the new default, on"
        );
        assert_eq!(
            serde_yaml::to_value(UserConfig::load(&path).unwrap()).unwrap(),
            serde_yaml::to_value(config).unwrap(),
            "the sparse config loads back to the same config"
        );
    }

    #[test]
    fn load_writes_a_sparse_config_with_sets_and_partial_sections() {
        // IBD peers serialize in no particular order, and a partial blend
        // backend must keep the defaults of the fields it omits.
        let config = CONFIG_0_3_2
            .replace(
                "        peers: []",
                "        peers:
        - 12D3KooWFrouXfmrR4nsLMtE7wu15DoMJ6VtoUtHinREZCvbWHar
        - 12D3KooWJRGau8M1rjT7R5e4YYsgdFhsMX35nRDtMwCDjxQkXAHz
        - 12D3KooWQXJavMDTRscjauFSgVAB1VLB6Rzpy2uY5SU9Tk7927tb",
            )
            .replace("udp/3400/quic-v1", "udp/3500/quic-v1");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_config.yaml");
        std::fs::write(&path, config).unwrap();
        std::fs::write(dir.path().join("keystore.yaml"), KEYSTORE_0_3_2).unwrap();

        UserConfig::load(&path).unwrap();

        let written: Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            written.get("kms").is_none(),
            "the config was written sparse"
        );
        assert_eq!(
            written["blend"]["core"]["backend"],
            yaml("{listening_address: /ip4/0.0.0.0/udp/3500/quic-v1}")
        );
        assert_eq!(
            written["cryptarchia"]["network"]["bootstrap"]["ibd"]["peers"]
                .as_sequence()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            UserConfig::load(&path)
                .unwrap()
                .blend
                .core
                .backend
                .peering_degree_check_interval,
            Some(Duration::from_mins(1))
        );
    }

    #[test]
    fn load_keeps_an_existing_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user_config.yaml");
        std::fs::write(&path, CONFIG_0_3_2).unwrap();
        std::fs::write(dir.path().join("user_config.yaml.0.3.2.bak"), "older").unwrap();

        UserConfig::load(&path).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("user_config.yaml.0.3.2.bak")).unwrap(),
            "older"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("user_config.yaml.0.3.2.bak.1")).unwrap(),
            CONFIG_0_3_2
        );
    }

    #[test]
    fn current_config_is_left_untouched() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../standalone-node-config.yaml");
        let (_, changed) = migrated(&std::fs::read_to_string(path).unwrap());
        assert!(!changed);
    }

    #[test]
    fn renames_unit_suffixed_durations() {
        let (config, changed) = migrated(
            "network: {backend: {swarm: {
                kademlia: {query_timeout_secs: 10, periodic_bootstrap_interval_secs: null},
                identify: {interval_secs: 300},
                nat: {type: traversal, autonat: {probe_interval_millisecs: 1500}}}}}",
        );
        assert!(changed);
        assert_eq!(
            config,
            yaml(
                "network: {backend: {swarm: {
                kademlia: {query_timeout: 10s, periodic_bootstrap_interval: null},
                identify: {interval: 5m},
                nat: {type: traversal, autonat: {probe_interval: 1s 500ms}}}}}"
            )
        );
    }

    #[test]
    fn translates_kademlia_caching() {
        let kademlia = |caching: &str| {
            let (config, changed) = migrated(&format!(
                "network: {{backend: {{swarm: {{kademlia: {{caching: {caching}}}}}}}}}"
            ));
            assert!(changed);
            config["network"]["backend"]["swarm"]["kademlia"].clone()
        };
        assert_eq!(kademlia("null"), yaml("{}"));
        assert_eq!(kademlia("{type: Disabled}"), yaml("{cache_size: null}"));
        assert_eq!(
            kademlia("{type: Enabled, config: {max_peers: 5}}"),
            yaml("{cache_size: 5}")
        );
    }

    #[test]
    fn translates_open_metrics_and_nested_durations() {
        let (config, changed) = migrated(
            "tracing:
               metrics: {enable_open_metrics: false}
               logger:
                 file:
                   appender_type: !Rolling
                     compression: !Gzip
                       compression_threshold: {secs: 7200, nanos: 0}",
        );
        assert!(changed);
        assert_eq!(
            config,
            yaml(
                "tracing:
               metrics: {}
               logger:
                 file:
                   appender_type: !Rolling
                     compression: !Gzip
                       compression_threshold: 2h"
            )
        );
    }

    #[test]
    fn keeps_a_customised_log_filter() {
        let config = CONFIG_0_3_2.replace("logos_blockchain: INFO", "logos_blockchain: DEBUG");
        let (config, changed) = migrated(&config);
        assert!(changed);
        assert_eq!(
            config["tracing"]["filter"]["filters"]["logos_blockchain"],
            Value::String("DEBUG".into())
        );
    }

    #[test]
    fn leaves_the_log_filter_of_a_current_config_alone() {
        let (config, changed) = migrated(
            "tracing:
               filter: !Env
                 filters: {'*': WARN, tower: WARN, rustls: WARN, tonic: WARN, h2: WARN,
                   opentelemetry: WARN, opentelemetry_sdk: WARN, reqwest: WARN,
                   libp2p_gossipsub: ERROR, logos_blockchain: INFO, hyper_util: WARN,
                   hyper: WARN}",
        );
        assert!(!changed);
        assert!(config["tracing"].get("filter").is_some());
    }

    #[test]
    fn leaves_other_strings_and_mappings_alone() {
        let (_, changed) = migrated(
            "{storage: {backend: {folder_name: '1.000000000x'}},
              time: {backend: {server: 'pool.ntp.org:123'}},
              other: {secs: 1, nanos: 0, extra: true}}",
        );
        assert!(!changed);
    }
}
