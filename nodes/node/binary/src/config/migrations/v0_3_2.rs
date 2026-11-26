//! 0.3.2 → the next release.
//!
//! 0.3.2 wrote every duration as serde's `{secs, nanos}` or as a
//! `"<secs>.<9 digit nanos>"` string, a few as bare integers, some under
//! unit-suffixed keys; they are all humantime strings (`5s`, `250ms`) now.
//! Kademlia's `caching` became `cache_size`, `enable_open_metrics` and the
//! leader wallet's `max_tx_fee` are gone and the built-in log filter 0.3.2
//! wrote into every config is no longer written.
//!
//! The public key of a note was `zkhash(KDF, sk)` up to 0.3.6 and is derived
//! from the nullifier key of `sk` now. Every public key a config names of a
//! secret key of the config or of the keystore is derived again, as are the
//! key ids made of one. A `zk_id` is still `zkhash(KDF, sk)` and is kept.
//!
//! 0.3.2 also wrote every default into the config. Those still the default are
//! dropped when the upgraded config is written sparse; those that aren't,
//! [`STALE_DEFAULTS`] and the generated log file prefix, are dropped here, so
//! the current default applies. A value someone set to a 0.3.2 default can't
//! be told apart from it.
//!
//! Every step also accepts the new form, so a config already in it comes out
//! unchanged; everything the steps don't name passes through untouched.

use core::{mem, time::Duration};
use std::collections::HashMap;

use color_eyre::eyre::{Result, WrapErr as _, eyre};
use lb_key_management_system_service::{
    backend::preload::KeyId,
    keys::{Key, kdf_public_key},
};
use lb_tracing::filter::envfilter::default_envfilter_config;
use serde::Deserialize;
use serde_yaml::{Mapping, Value, to_value};
use tracing::Level;

use crate::config::{TracingConfig, keystore::Keystore, tracing::serde::filter::Layer};

const KADEMLIA: &[&str] = &["network", "backend", "swarm", "kademlia"];
const CHAIN_SYNC: &[&str] = &["network", "backend", "swarm", "chain_sync"];

/// Durations 0.3.2 wrote as bare integers: their section, their key then and
/// now, and their unit in milliseconds.
const INTEGER_DURATIONS: &[(&[&str], &str, &str, u64)] = &[
    (&["api", "backend"], "timeout", "timeout", 1000),
    (KADEMLIA, "query_timeout_secs", "query_timeout", 1000),
    (
        KADEMLIA,
        "periodic_bootstrap_interval_secs",
        "periodic_bootstrap_interval",
        1000,
    ),
    (
        &["network", "backend", "swarm", "identify"],
        "interval_secs",
        "interval",
        1000,
    ),
    (
        CHAIN_SYNC,
        "peer_response_timeout",
        "peer_response_timeout",
        1,
    ),
    // Not in 0.3.2, but written as milliseconds by early 0.3.3 builds.
    (
        CHAIN_SYNC,
        "inbound_download_stream_timeout",
        "inbound_download_stream_timeout",
        1,
    ),
    (
        &["network", "backend", "swarm", "nat", "autonat"],
        "probe_interval_millisecs",
        "probe_interval",
        1,
    ),
];

/// 0.3.2 defaults that are not the current ones, as 0.3.2 wrote them: the
/// mempool `tx_ttl` went from 9h to 1h, and a `null` kademlia `caching` was
/// libp2p's default, which `cache_size` now defaults to. The upgrade test fails
/// on any other 0.3.2 default left in the upgraded config.
const STALE_DEFAULTS: &[(&[&str], &str, &str)] = &[
    (&["mempool"], "tx_ttl", "'32400.000000000'"),
    (KADEMLIA, "caching", "null"),
];

/// 0.3.2 `caching: {type: Disabled} | {type: Enabled, config: {max_peers}}`,
/// now `cache_size: null | <max_peers>`.
#[derive(Deserialize)]
#[serde(tag = "type", content = "config")]
enum Caching {
    Disabled,
    Enabled { max_peers: u16 },
}

/// serde's encoding of [`Duration`], which 0.3.2 wrote for most durations.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StdDuration {
    secs: u64,
    nanos: u32,
}

pub fn migrate(config: &mut Value, keystore: Option<&Keystore>) -> Result<()> {
    let original = config.clone();
    for (section, key, default) in STALE_DEFAULTS {
        if let Some(section) = mapping_at(config, section)
            && section.get(*key) == Some(&serde_yaml::from_str(default)?)
        {
            section.remove(*key);
        }
    }
    if let Some(metrics) = mapping_at(config, &["tracing", "metrics"]) {
        // Dropped rather than translated: 0.3.2 wrote its `false` default into
        // every config, and upgraded nodes get the new always-on default.
        metrics.remove("enable_open_metrics");
    }
    if let Some(wallet) = mapping_at(config, &["cryptarchia", "leader", "wallet"]) {
        // The fee cap of the leader's reward claims, which a leader no longer
        // makes: its reward is a note of the block it proposes.
        wallet.remove("max_tx_fee");
    }
    humanize_std_durations(config);
    for (section, old, new, unit_ms) in INTEGER_DURATIONS {
        let Some(section) = mapping_at(config, section) else {
            continue;
        };
        let Some(value) = section.remove(*old).or_else(|| section.remove(*new)) else {
            continue;
        };
        let value = match value {
            Value::Number(units) => {
                let units = units
                    .as_u64()
                    .ok_or_else(|| eyre!("`{old}`: {units} is not a whole, positive duration"))?;
                humantime(Duration::from_millis(units * unit_ms))
            }
            value => value,
        };
        section.insert((*new).into(), value);
    }
    if let Some(kademlia) = mapping_at(config, KADEMLIA)
        && let Some(caching) = kademlia.remove("caching")
    {
        let cache_size = match Caching::deserialize(caching).wrap_err("reading `caching`")? {
            Caching::Disabled => Value::Null,
            Caching::Enabled { max_peers } => max_peers.into(),
        };
        kademlia.insert("cache_size".into(), cache_size);
    }
    // Only alongside other changes: a current config may have them on purpose.
    if *config != original {
        drop_built_in_log_filter(config);
        drop_generated_log_prefix(config);
    }
    rederive_note_public_keys(config, keystore)
}

/// Replaces the public key every ZK secret key of the config and of `keystore`
/// had up to 0.3.6 by its current one, wherever the config names it: as a
/// public key or as the id of the key.
fn rederive_note_public_keys(config: &mut Value, keystore: Option<&Keystore>) -> Result<()> {
    let config_keys: HashMap<KeyId, Key> = config
        .get("kms")
        .and_then(|kms| kms.get("backend"))
        .and_then(|backend| backend.get("keys"))
        .cloned()
        .map(serde_yaml::from_value)
        .transpose()
        .wrap_err("reading `kms.backend.keys`")?
        .unwrap_or_default();
    let rederived = config_keys
        .values()
        .chain(keystore.into_iter().flat_map(Keystore::secret_keys))
        .filter_map(|key| match key {
            Key::Zk(key) if key.is_spending_key() => {
                Some((kdf_public_key(key.as_fr()), key.to_public_key()))
            }
            _ => None,
        })
        .map(|(old, new)| Ok((to_value(old)?, to_value(new)?)))
        .collect::<Result<HashMap<_, _>>>()?;
    replace_public_keys(config, &rederived);
    Ok(())
}

fn replace_public_keys(value: &mut Value, rederived: &HashMap<Value, Value>) {
    match value {
        Value::Mapping(mapping) => {
            *mapping = mem::take(mapping)
                .into_iter()
                .map(|(key, mut value)| {
                    if key.as_str() != Some("zk_id") {
                        replace_public_keys(&mut value, rederived);
                    }
                    (rederived.get(&key).cloned().unwrap_or(key), value)
                })
                .collect();
        }
        Value::Sequence(values) => values
            .iter_mut()
            .for_each(|value| replace_public_keys(value, rederived)),
        Value::Tagged(tagged) => replace_public_keys(&mut tagged.value, rederived),
        public_key => {
            if let Some(rederived) = rederived.get(public_key) {
                *public_key = rederived.clone();
            }
        }
    }
}

fn mapping_at<'a>(config: &'a mut Value, path: &[&str]) -> Option<&'a mut Mapping> {
    path.iter()
        .try_fold(config, |value, key| value.get_mut(*key))?
        .as_mapping_mut()
}

/// Rewrites the two encodings 0.3.2 wrote for most durations, wherever they
/// are: `{secs, nanos}` and `"<secs>.<9 digit nanos>"`. Nothing else in a
/// config takes either shape.
fn humanize_std_durations(value: &mut Value) {
    let duration = match &*value {
        Value::Mapping(_) => StdDuration::deserialize(&*value)
            .ok()
            .map(|StdDuration { secs, nanos }| Duration::new(secs, nanos)),
        Value::String(string) => string.split_once('.').and_then(|(secs, nanos)| {
            let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            (digits(secs) && nanos.len() == 9 && digits(nanos))
                .then(|| Some(Duration::new(secs.parse().ok()?, nanos.parse().ok()?)))
                .flatten()
        }),
        _ => None,
    };
    if let Some(duration) = duration {
        *value = humantime(duration);
        return;
    }
    match value {
        Value::Mapping(mapping) => mapping.values_mut().for_each(humanize_std_durations),
        Value::Sequence(values) => values.iter_mut().for_each(humanize_std_durations),
        Value::Tagged(tagged) => humanize_std_durations(&mut tagged.value),
        _ => {}
    }
}

fn humantime(duration: Duration) -> Value {
    Value::String(humantime::format_duration(duration).to_string())
}

/// 0.3.2 named the log files after the time `init-config` ran, in Unix
/// seconds; they aren't prefixed by default now.
fn drop_generated_log_prefix(config: &mut Value) {
    if let Some(file) = mapping_at(config, &["tracing", "logger", "file"])
        && file
            .get("prefix")
            .and_then(Value::as_str)
            .is_some_and(|prefix| !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()))
    {
        file.remove("prefix");
    }
}

/// 0.3.2 wrote the built-in log filter of the configured level into every
/// config. Unchanged, it's dropped, so the node follows the built-in filter of
/// the release it runs, which applies when none is configured.
fn drop_built_in_log_filter(config: &mut Value) {
    let Some(tracing) = mapping_at(config, &["tracing"]) else {
        return;
    };
    let level = tracing.get("level").map_or_else(
        || Some(TracingConfig::default().level),
        |level| level.as_str()?.parse::<Level>().ok(),
    );
    let configured = tracing
        .get("filter")
        .and_then(|filter| serde_yaml::from_value::<Layer>(filter.clone()).ok());
    if let (Some(level), Some(Layer::Env(configured))) = (level, configured)
        && default_envfilter_config(level)
            .is_some_and(|built_in| built_in.filters == configured.filters)
    {
        tracing.remove("filter");
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_service::keys::ZkKey;
    use serde_yaml::value::{Tag, TaggedValue};

    use super::*;

    fn yaml(yaml: &str) -> Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn direct_credentials_do_not_override_spending_key_rederivation() {
        let spending = ZkKey::new(73u64.into());
        let direct = ZkKey::from_direct_secret(*spending.as_fr());
        let old_public_key = kdf_public_key(spending.as_fr());
        assert_eq!(direct.to_public_key(), old_public_key);
        let keys = HashMap::from([("spending".to_owned(), Key::Zk(spending.clone()))]);
        let mut store = yaml("public_keys: {}\nsecret_keys: {}\nWARNING: test\n");
        store["secret_keys"] = to_value(HashMap::from([("direct", Key::Zk(direct))])).unwrap();
        let store: Keystore = serde_yaml::from_value(store).unwrap();
        let mut config = yaml("kms: {backend: {keys: {}}}\nwallet: {funding: null}\n");
        config["kms"]["backend"]["keys"] = to_value(keys).unwrap();
        config["wallet"]["funding"] = to_value(old_public_key).unwrap();
        rederive_note_public_keys(&mut config, Some(&store)).unwrap();
        assert_eq!(
            config["wallet"]["funding"],
            to_value(spending.to_public_key()).unwrap()
        );
    }

    #[test]
    fn rewrites_a_0_3_2_config_into_the_new_format() {
        let mut config = yaml("
api: {backend: {timeout: 45, max_body_size: 10}}
network:
  backend:
    swarm:
      gossipsub: {heartbeat_interval: {secs: 1, nanos: 0}}
      kademlia: {query_timeout_secs: 90, periodic_bootstrap_interval_secs: null, caching: {type: Enabled, config: {max_peers: 7}}}
      identify: {interval_secs: 300}
      chain_sync: {peer_response_timeout: 1500}
      nat: {autonat: {probe_interval_millisecs: 250}, mapping: {timeout: '1.500000000'}}
mempool: {tx_ttl: '32400.000000000', seen_items_subscription_channel_size: 64}
tracing:
  level: INFO
  logger: {file: {prefix: '1793708149'}}
  metrics: {enable_open_metrics: false}
");
        config["tracing"]["filter"] = Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new("Env"),
            value: to_value(default_envfilter_config(Level::INFO).unwrap()).unwrap(),
        }));
        migrate(&mut config, None).unwrap();
        assert_eq!(
            config,
            yaml(
                "
api: {backend: {timeout: 45s, max_body_size: 10}}
network:
  backend:
    swarm:
      gossipsub: {heartbeat_interval: 1s}
      kademlia: {query_timeout: 1m 30s, periodic_bootstrap_interval: null, cache_size: 7}
      identify: {interval: 5m}
      chain_sync: {peer_response_timeout: 1s 500ms}
      nat: {autonat: {probe_interval: 250ms}, mapping: {timeout: 1s 500ms}}
mempool: {seen_items_subscription_channel_size: 64}
tracing:
  level: INFO
  logger: {file: {}}
  metrics: {}
"
            )
        );
        // Already upgraded, nothing changes.
        let upgraded = config.clone();
        migrate(&mut config, None).unwrap();
        assert_eq!(config, upgraded);

        // Disabled caching, a custom prefix and a customised filter are kept.
        let mut config = yaml(
            "
network: {backend: {swarm: {kademlia: {caching: {type: Disabled}}}}}
tracing: {logger: {file: {prefix: node-a}}, filter: !Env {filters: {'*': DEBUG}}}
",
        );
        migrate(&mut config, None).unwrap();
        assert_eq!(
            config,
            yaml(
                "
network: {backend: {swarm: {kademlia: {cache_size: null}}}}
tracing: {logger: {file: {prefix: node-a}}, filter: !Env {filters: {'*': DEBUG}}}
"
            )
        );
    }
}
