///  Handles Cucumber config overrides for manual-node tests.
/*
  Overview
  --------
  Step values are collected as YAML fragments and later patched into the
  user/deployment config by path. Most values use normal YAML parsing, so
  booleans, numbers, strings, sequences, and mappings can be written directly
  in test steps.

  Supported explicit override functions
  -------------------------------------
  - hex(...) Marks a string as hex input. When applied to a byte-sequence
    field, it is decoded into bytes.
  - seconds(...) Marks a duration value in seconds. This is converted at apply
    time into the YAML shape required by the target duration field.
  - `now_plus_seconds`(...) Produces an `OffsetDateTime` relative to the current
    UTC time.

  Apply-time coercion
  -------------------
  A small amount of target-aware coercion remains necessary when patching
  YAML:
  - duration-like fields accept seconds(...)
  - byte-sequence fields accept hex(...) and plain human-readable strings,
    which are encoded as UTF-8 bytes

  Design intent
  -------------
  The file favors explicit step syntax over broad implicit magic:
  - plain YAML stays plain YAML
  - special conversions use named functions
  - only a narrow amount of destination-aware coercion is kept where the final
    YAML representation depends on the target field type

  This keeps override steps readable while avoiding hidden parsing rules.

  Examples:
  ---------

  And I have deployment config override "time.chain_start_time" as "now_plus_seconds(0)"
  And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(2)"

  # Duration examples
  And I have user config override "cryptarchia.service.bootstrap.prolonged_bootstrap_period" as "seconds(1.2)"
  And I have user config override "network.backend.swarm.gossipsub.heartbeat_interval" as "seconds(1)"

  # Byte / inscription examples
  And I have deployment config override "cryptarchia.genesis_state.mantle_tx.ops.1.payload.inscription" as "hex(70726f636573735f73746172745f6e6f6e6365)"
  And I have deployment config override "cryptarchia.genesis_state.mantle_tx.ops.1.payload.inscription" as "process_start_nonce"

  # Scalar YAML (no function needed)
  And I have user config override "network.backend.swarm.gossipsub.validate_messages" as "true"
  And I have user config override "network.backend.swarm.gossipsub.history_length" as "5"
  And I have user config override "network.backend.swarm.gossipsub.gossip_factor" as "0.5"

  # Strings
  And I have deployment config override "mempool.pubsub_topic" as "my-custom-topic"

  # Complex string (parsed later into Multiaddr etc.)
  And I have user config override "blend.core.backend.listening_address" as "/ip4/127.0.0.1/udp/20128/quic-v1"
*/
///
use lb_node::config::RunConfig;
use serde::{Serialize, de::DeserializeOwned};
use serde_yaml::{Mapping, Value as YamlValue};
use time::{Duration as TimeDuration, OffsetDateTime};

use crate::cucumber::{
    error::{StepError, StepResult},
    world::{ConfigOverride, CucumberWorld},
};

// ============================================================
// Public API
// ============================================================

pub fn set_user_config_override(
    world: &mut CucumberWorld,
    step: &str,
    raw_path: &str,
    raw_value: &str,
) -> StepResult {
    set_override(&mut world.user_config_overrides, step, raw_path, raw_value)
}

pub fn set_deployment_config_override(
    world: &mut CucumberWorld,
    step: &str,
    raw_path: &str,
    raw_value: &str,
) -> StepResult {
    set_override(
        &mut world.deployment_config_overrides,
        step,
        raw_path,
        raw_value,
    )
}

pub fn apply_user_config_overrides(
    config: &mut RunConfig,
    overrides: &[ConfigOverride],
) -> Result<(), StepError> {
    apply_overrides(&mut config.user, overrides, "user")
}

pub fn apply_deployment_config_overrides(
    config: &mut RunConfig,
    overrides: &[ConfigOverride],
) -> Result<(), StepError> {
    apply_overrides(&mut config.deployment, overrides, "deployment")
}

// ============================================================
// Override collection
// ============================================================

fn set_override(
    overrides: &mut Vec<ConfigOverride>,
    step: &str,
    raw_path: &str,
    raw_value: &str,
) -> StepResult {
    let path = normalize_path(step, raw_path)?;
    let value = parse_value(step, raw_value)?;

    if let Some(existing) = overrides.iter_mut().find(|item| item.path == path) {
        existing.value = value;
    } else {
        overrides.push(ConfigOverride { path, value });
    }

    Ok(())
}

fn normalize_path(step: &str, raw_path: &str) -> Result<String, StepError> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(step_error(step, "config path must not be empty"));
    }

    let segments = trimmed
        .split('.')
        .map(str::trim)
        .map(|segment| {
            if segment.is_empty() {
                Err(step_error(
                    step,
                    &format!("config path '{trimmed}' must not contain empty segments"),
                ))
            } else {
                Ok(segment)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(segments.join("."))
}

fn parse_value(step: &str, raw_value: &str) -> Result<YamlValue, StepError> {
    let raw = raw_value.trim();

    if let Some((name, arg)) = parse_call(raw) {
        return match name {
            "hex" => parse_hex(arg, step, raw),
            "seconds" => parse_seconds(arg, step, raw),
            "now_plus_seconds" => parse_now_plus_seconds_value(arg, step, raw),
            _ => Err(step_error(
                step,
                &format!("unknown override function '{name}' in '{raw}'"),
            )),
        };
    }

    serde_yaml::from_str::<YamlValue>(raw)
        .map_or_else(|_| Ok(YamlValue::String(raw.to_owned())), Ok)
}

fn parse_call(raw: &str) -> Option<(&str, &str)> {
    let raw = raw.strip_suffix(')')?;
    let (name, arg) = raw.split_once('(')?;
    let name = name.trim();
    let arg = arg.trim();

    if name.is_empty() {
        return None;
    }

    Some((name, arg))
}

fn parse_named_call<'a>(raw: &'a str, expected_name: &str) -> Option<&'a str> {
    let (name, arg) = parse_call(raw)?;
    (name == expected_name).then_some(arg)
}

fn parse_hex(arg: &str, step: &str, raw: &str) -> Result<YamlValue, StepError> {
    let hex = arg.trim().trim_start_matches("0x").trim_start_matches("0X");
    if !is_hex_string(hex) {
        return Err(step_error(step, &format!("invalid hex override '{raw}'")));
    }

    Ok(YamlValue::String(hex.to_owned()))
}

fn parse_seconds(arg: &str, step: &str, raw: &str) -> Result<YamlValue, StepError> {
    let (seconds, nanos) = parse_seconds_parts(arg)
        .ok_or_else(|| step_error(step, &format!("invalid seconds override '{raw}'")))?;

    if seconds < 0 {
        return Err(step_error(
            step,
            &format!("negative seconds override '{raw}' is not supported"),
        ));
    }

    Ok(YamlValue::String(format!("seconds({seconds}.{nanos:09})")))
}

fn parse_now_plus_seconds_value(arg: &str, step: &str, raw: &str) -> Result<YamlValue, StepError> {
    let seconds = arg
        .parse::<i64>()
        .map_err(|_| step_error(step, &format!("invalid now_plus_seconds override '{raw}'")))?;

    let timestamp = OffsetDateTime::now_utc() + TimeDuration::seconds(seconds);
    serde_yaml::to_value(timestamp).map_err(|source| {
        step_error(
            step,
            &format!("failed to convert override '{raw}' to YAML: {source}"),
        )
    })
}

// ============================================================
// Override application
// ============================================================

fn apply_overrides<T>(
    target: &mut T,
    overrides: &[ConfigOverride],
    scope: &str,
) -> Result<(), StepError>
where
    T: Serialize + DeserializeOwned,
{
    if overrides.is_empty() {
        return Ok(());
    }

    let mut yaml = to_yaml(target, scope)?;

    for ov in overrides {
        apply_override(&mut yaml, ov)?;
    }

    from_yaml(yaml, scope, target)
}

fn to_yaml<T>(target: &T, scope: &str) -> Result<YamlValue, StepError>
where
    T: Serialize,
{
    serde_yaml::to_value(target).map_err(|source| StepError::LogicalError {
        message: format!("failed to serialize {scope} config for patching: {source}"),
    })
}

fn from_yaml<T>(yaml: YamlValue, scope: &str, target: &mut T) -> Result<(), StepError>
where
    T: DeserializeOwned,
{
    *target = serde_yaml::from_value(yaml).map_err(|source| StepError::InvalidArgument {
        message: format!(
            "invalid {scope} config override: resulting config could not be deserialized: {source}"
        ),
    })?;

    Ok(())
}

fn apply_override(root: &mut YamlValue, ov: &ConfigOverride) -> Result<(), StepError> {
    let path = split_path(&ov.path);
    let value = coerce_at_path(root, &path, ov.value.clone(), &ov.path)?;
    set_at_path(root, &path, value, &ov.path)
}

fn split_path(path: &str) -> Vec<&str> {
    path.split('.').collect()
}

// ============================================================
// Type-aware coercion
// ============================================================

fn coerce_at_path(
    root: &YamlValue,
    path: &[&str],
    value: YamlValue,
    full_path: &str,
) -> Result<YamlValue, StepError> {
    let Some(existing) = get_at_path(root, path) else {
        return Ok(value);
    };

    if is_duration_like_yaml(existing) {
        return coerce_seconds(existing, value, full_path);
    }

    if is_byte_sequence_like_yaml(existing) {
        return coerce_string_to_bytes(value, full_path);
    }

    Ok(value)
}

fn coerce_seconds(
    existing: &YamlValue,
    value: YamlValue,
    full_path: &str,
) -> Result<YamlValue, StepError> {
    let Some(raw) = value.as_str() else {
        return Ok(value);
    };

    let Some(arg) = parse_named_call(raw, "seconds") else {
        return Ok(value);
    };

    let (seconds, nanos) = parse_seconds_parts(arg)
        .ok_or_else(|| invalid_path(full_path, &format!("invalid seconds override '{raw}'")))?;

    if seconds < 0 {
        return Err(invalid_path(
            full_path,
            &format!("negative seconds override '{raw}' is not supported"),
        ));
    }

    Ok(duration_yaml(existing, seconds, nanos))
}

fn coerce_string_to_bytes(value: YamlValue, full_path: &str) -> Result<YamlValue, StepError> {
    let Some(raw) = value.as_str() else {
        return Ok(value);
    };

    let bytes = if is_hex_string(raw) {
        hex::decode(raw).map_err(|source| {
            invalid_path(full_path, &format!("invalid hex bytes '{raw}': {source}"))
        })?
    } else {
        raw.as_bytes().to_vec()
    };

    Ok(YamlValue::Sequence(
        bytes
            .into_iter()
            .map(|byte| YamlValue::from(i64::from(byte)))
            .collect(),
    ))
}

// ============================================================
// Duration helpers
// ============================================================

fn parse_seconds_parts(raw: &str) -> Option<(i64, u32)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some((seconds, fraction)) = raw.split_once('.') {
        if !is_ascii_digits(seconds) || !is_ascii_digits(fraction) {
            return None;
        }

        let seconds = seconds.parse::<i64>().ok()?;
        let nanos = normalize_nanos(fraction).parse::<u32>().ok()?;
        return Some((seconds, nanos));
    }

    if is_ascii_digits(raw) {
        return Some((raw.parse::<i64>().ok()?, 0));
    }

    None
}

fn duration_yaml(existing: &YamlValue, seconds: i64, nanos: u32) -> YamlValue {
    match existing {
        YamlValue::Sequence(_) => YamlValue::Sequence(vec![
            YamlValue::from(seconds),
            YamlValue::from(i64::from(nanos)),
        ]),
        YamlValue::Mapping(_) => {
            let mut map = Mapping::new();
            map.insert(
                YamlValue::String("secs".to_owned()),
                YamlValue::from(seconds),
            );
            map.insert(
                YamlValue::String("nanos".to_owned()),
                YamlValue::from(i64::from(nanos)),
            );
            YamlValue::Mapping(map)
        }
        _ => YamlValue::String(format!("{seconds}.{nanos:09}")),
    }
}

fn is_duration_like_yaml(value: &YamlValue) -> bool {
    match value {
        YamlValue::String(v) => is_duration_string(v),
        YamlValue::Sequence(v) => {
            v.len() == 2
                && yaml_integer_to_i64(&v[0]).is_some()
                && yaml_integer_to_i64(&v[1]).is_some()
        }
        YamlValue::Mapping(v) => {
            let secs = v
                .get(YamlValue::String("secs".to_owned()))
                .and_then(yaml_integer_to_i64);
            let nanos = v
                .get(YamlValue::String("nanos".to_owned()))
                .and_then(yaml_integer_to_i64);
            secs.is_some() && nanos.is_some()
        }
        _ => false,
    }
}

fn is_duration_string(value: &str) -> bool {
    let Some((secs, nanos)) = value.split_once('.') else {
        return false;
    };

    is_ascii_digits(secs) && nanos.len() == 9 && is_ascii_digits(nanos)
}

// ============================================================
// YAML traversal
// ============================================================

fn get_at_path<'a>(current: &'a YamlValue, path: &[&str]) -> Option<&'a YamlValue> {
    let mut current = current;

    for segment in path {
        current = if let Ok(index) = segment.parse::<usize>() {
            current.as_sequence()?.get(index)?
        } else {
            current
                .as_mapping()?
                .get(YamlValue::String((*segment).to_owned()))?
        };
    }

    Some(current)
}

fn set_at_path(
    current: &mut YamlValue,
    path: &[&str],
    value: YamlValue,
    full_path: &str,
) -> Result<(), StepError> {
    if path.is_empty() {
        *current = value;
        return Ok(());
    }

    let segment = path[0];
    let rest = &path[1..];
    let is_last = rest.is_empty();

    if let Ok(index) = segment.parse::<usize>() {
        return set_seq(current, segment, index, rest, value, full_path, is_last);
    }

    set_map(current, segment, rest, value, full_path, is_last)
}

fn set_seq(
    current: &mut YamlValue,
    segment: &str,
    index: usize,
    rest: &[&str],
    value: YamlValue,
    full_path: &str,
    is_last: bool,
) -> Result<(), StepError> {
    if current.is_null() {
        *current = YamlValue::Sequence(Vec::new());
    }

    let sequence = current.as_sequence_mut().ok_or_else(|| {
        invalid_path(
            full_path,
            &format!("segment '{segment}' expects a YAML sequence"),
        )
    })?;

    if sequence.len() <= index {
        sequence.resize(index + 1, YamlValue::Null);
    }

    if is_last {
        sequence[index] = value;
        return Ok(());
    }

    set_at_path(&mut sequence[index], rest, value, full_path)
}

fn set_map(
    current: &mut YamlValue,
    segment: &str,
    rest: &[&str],
    value: YamlValue,
    full_path: &str,
    is_last: bool,
) -> Result<(), StepError> {
    if current.is_null() {
        *current = YamlValue::Mapping(Mapping::new());
    }

    let mapping = current.as_mapping_mut().ok_or_else(|| {
        invalid_path(
            full_path,
            &format!("segment '{segment}' expects a YAML mapping"),
        )
    })?;

    let key = YamlValue::String(segment.to_owned());
    if is_last {
        mapping.insert(key, value);
        return Ok(());
    }

    let child = mapping.entry(key).or_insert_with(|| default_child(rest));
    if child.is_null() {
        *child = default_child(rest);
    }

    set_at_path(child, rest, value, full_path)
}

fn default_child(rest: &[&str]) -> YamlValue {
    if rest
        .first()
        .is_some_and(|segment| segment.parse::<usize>().is_ok())
    {
        YamlValue::Sequence(Vec::new())
    } else {
        YamlValue::Mapping(Mapping::new())
    }
}

// ============================================================
// Small utilities
// ============================================================

fn yaml_integer_to_i64(value: &YamlValue) -> Option<i64> {
    value.as_i64().or_else(|| {
        let u = value.as_u64()?;
        i64::try_from(u).ok()
    })
}

fn normalize_nanos(fraction: &str) -> String {
    if fraction.len() >= 9 {
        fraction.chars().take(9).collect()
    } else {
        format!("{fraction:0<9}")
    }
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

fn is_byte_sequence_like_yaml(value: &YamlValue) -> bool {
    let Some(values) = value.as_sequence() else {
        return false;
    };

    !values.is_empty()
        && values
            .iter()
            .all(|v| v.as_i64().is_some_and(|n| (0..=255).contains(&n)))
}

fn is_hex_string(value: &str) -> bool {
    !value.is_empty()
        && value.len().is_multiple_of(2)
        && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn step_error(step: &str, detail: &str) -> StepError {
    StepError::InvalidArgument {
        message: format!("step `{step}`: {detail}"),
    }
}

fn invalid_path(path: &str, detail: &str) -> StepError {
    StepError::InvalidArgument {
        message: format!("invalid config override at '{path}': {detail}"),
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lb_libp2p::Multiaddr;

    use super::*;
    use crate::{
        add_strings,
        nodes::create_validator_config,
        topology::configs::{
            create_general_configs, deployment::e2e_deployment_settings_with_genesis_tx,
        },
    };

    #[test]
    fn normalize_path_rejects_empty_segments() {
        let path = "network..backend";
        assert!(normalize_path("test-step", path).is_err());
    }

    #[test]
    fn parse_value_supports_seconds_function() {
        let value = parse_value("test-step", "seconds(1.2)").expect("seconds override");
        assert_eq!(value.as_str(), Some("seconds(1.200000000)"));
    }

    #[test]
    fn parse_value_supports_now_plus_seconds_function() {
        let value = parse_value("test-step", "now_plus_seconds(10)").expect("now override");
        let _ = serde_yaml::from_value::<OffsetDateTime>(value).expect("timestamp");
    }

    #[test]
    fn parse_value_supports_hex_function() {
        let value = parse_value("test-step", "hex(deadbeef)").expect("hex override");
        assert_eq!(value.as_str(), Some("deadbeef"));
    }

    #[test]
    fn set_at_path_supports_nested_mapping() {
        let mut root = YamlValue::Mapping(Mapping::new());
        set_at_path(
            &mut root,
            &["network", "backend", "swarm", "gossipsub", "retain_scores"],
            serde_yaml::from_str::<YamlValue>("20").expect("yaml value"),
            "network.backend.swarm.gossipsub.retain_scores",
        )
        .expect("path should be writable");

        let got = root["network"]["backend"]["swarm"]["gossipsub"]["retain_scores"].as_i64();
        assert_eq!(got, Some(20));
    }

    #[test]
    fn set_at_path_supports_sequence_indices() {
        let mut root = YamlValue::Mapping(Mapping::new());
        set_at_path(
            &mut root,
            &["network", "backend", "initial_peers", "1"],
            serde_yaml::from_str::<YamlValue>("/ip4/127.0.0.1/udp/3000/quic-v1")
                .expect("yaml value"),
            "network.backend.initial_peers.1",
        )
        .expect("path should be writable");

        let sequence = root["network"]["backend"]["initial_peers"]
            .as_sequence()
            .expect("sequence");
        assert_eq!(sequence.len(), 2);
        assert_eq!(
            sequence[1].as_str(),
            Some("/ip4/127.0.0.1/udp/3000/quic-v1")
        );
    }

    #[test]
    fn apply_overrides_updates_user_and_deployment_config() {
        let (configs, genesis_tx) = create_general_configs(1, Some("test_set_config_overrides"));
        let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
        let mut config = create_validator_config(configs[0].clone(), deployment_settings);

        let retain_scores = config.user.network.backend.swarm.gossipsub.retain_scores;
        let override_1 = ConfigOverride {
            path: "network.backend.swarm.gossipsub.retain_scores".to_owned(),
            value: (retain_scores + 10).into(),
        };
        let read_only = config.user.storage.backend.read_only;
        let override_2 = ConfigOverride {
            path: "storage.backend.read_only".to_owned(),
            value: serde_yaml::to_value(!read_only).expect("yaml value"),
        };
        let override_3 = ConfigOverride {
            path: "cryptarchia.service.bootstrap.prolonged_bootstrap_period".to_owned(),
            value: serde_yaml::to_value(TimeDuration::ZERO).expect("yaml value"),
        };
        assert!(
            apply_user_config_overrides(&mut config, &[override_1, override_2, override_3]).is_ok()
        );
        assert_eq!(
            config.user.network.backend.swarm.gossipsub.retain_scores,
            retain_scores + 10
        );
        assert_eq!(config.user.storage.backend.read_only, !read_only);
        assert_eq!(
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period,
            Duration::ZERO
        );

        let pubsub_topic = config.deployment.mempool.pubsub_topic.clone();
        let override_4 = ConfigOverride {
            path: "time.slot_duration".to_owned(),
            value: serde_yaml::to_value(TimeDuration::new(1, 0)).expect("yaml value"),
        };
        let override_5 = ConfigOverride {
            path: "mempool.pubsub_topic".to_owned(),
            value: serde_yaml::to_value(add_strings!(&[&pubsub_topic, "_test_1234"]))
                .expect("yaml value"),
        };
        assert!(apply_deployment_config_overrides(&mut config, &[override_4, override_5]).is_ok());
        assert_eq!(config.deployment.time.slot_duration, Duration::from_secs(1));
        assert_eq!(
            config.deployment.mempool.pubsub_topic,
            add_strings!(&[&pubsub_topic, "_test_1234"])
        );
    }

    #[test]
    fn world_overrides_accept_explicit_functions() {
        let (configs, genesis_tx) = create_general_configs(1, Some("test_override_functions"));
        let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
        let mut config = create_validator_config(configs[0].clone(), deployment_settings);
        let mut world = CucumberWorld::default();

        set_user_config_override(
            &mut world,
            "test-step",
            "cryptarchia.service.bootstrap.prolonged_bootstrap_period",
            "seconds(1.2)",
        )
        .expect("user duration override");
        set_user_config_override(
            &mut world,
            "test-step",
            "network.backend.swarm.gossipsub.heartbeat_interval",
            "seconds(1)",
        )
        .expect("user duration int override");
        set_deployment_config_override(&mut world, "test-step", "time.slot_duration", "seconds(1)")
            .expect("deployment duration override");
        set_deployment_config_override(
            &mut world,
            "test-step",
            "time.chain_start_time",
            "now_plus_seconds(10)",
        )
        .expect("deployment time override");

        apply_user_config_overrides(&mut config, &world.user_config_overrides)
            .expect("apply user overrides");
        apply_deployment_config_overrides(&mut config, &world.deployment_config_overrides)
            .expect("apply deployment overrides");

        assert_eq!(
            config
                .user
                .cryptarchia
                .service
                .bootstrap
                .prolonged_bootstrap_period,
            Duration::from_millis(1200)
        );
        assert_eq!(
            config
                .user
                .network
                .backend
                .swarm
                .gossipsub
                .heartbeat_interval,
            Duration::from_secs(1)
        );
        assert_eq!(config.deployment.time.slot_duration, Duration::from_secs(1));
    }

    #[test]
    fn world_overrides_round_trip_scalar_types() {
        let (configs, genesis_tx) = create_general_configs(1, Some("test_override_scalar_types"));
        let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
        let mut config = create_validator_config(configs[0].clone(), deployment_settings);
        let mut world = CucumberWorld::default();

        set_user_config_override(
            &mut world,
            "test-step",
            "network.backend.swarm.gossipsub.validate_messages",
            "true",
        )
        .expect("bool override");
        set_user_config_override(
            &mut world,
            "test-step",
            "cryptarchia.service.bootstrap.force_bootstrap",
            "true",
        )
        .expect("bool force_bootstrap override");
        set_user_config_override(
            &mut world,
            "test-step",
            "network.backend.swarm.gossipsub.history_length",
            "5",
        )
        .expect("usize override");
        set_user_config_override(
            &mut world,
            "test-step",
            "network.backend.swarm.gossipsub.gossip_factor",
            "0.5",
        )
        .expect("f64 override");
        set_deployment_config_override(
            &mut world,
            "test-step",
            "mempool.pubsub_topic",
            "my-custom-topic",
        )
        .expect("string override");
        set_user_config_override(
            &mut world,
            "test-step",
            "blend.core.backend.listening_address",
            "/ip4/127.0.0.1/udp/20128/quic-v1",
        )
        .expect("multiaddr override");

        set_user_config_override(
            &mut world,
            "test-step",
            "cryptarchia.leader.wallet.funding_pk",
            "hex(0000000000000000000000000000000000000000000000000000000000000000)",
        )
        .expect("zkpk hex string override");

        set_user_config_override(
            &mut world,
            "test-step",
            "network.backend.swarm.node_key",
            "hex(0101010101010101010101010101010101010101010101010101010101010101)",
        )
        .expect("node_key hex string override");

        apply_user_config_overrides(&mut config, &world.user_config_overrides)
            .expect("apply user overrides");
        apply_deployment_config_overrides(&mut config, &world.deployment_config_overrides)
            .expect("apply deployment overrides");

        assert!(
            config
                .user
                .network
                .backend
                .swarm
                .gossipsub
                .validate_messages
        );
        assert!(config.user.cryptarchia.service.bootstrap.force_bootstrap);
        assert_eq!(
            config.user.network.backend.swarm.gossipsub.history_length,
            5
        );
        assert!((config.user.network.backend.swarm.gossipsub.gossip_factor - 0.5f64).abs() < 1e-9);
        assert_eq!(config.deployment.mempool.pubsub_topic, "my-custom-topic");
        assert_eq!(
            config.user.blend.core.backend.listening_address,
            "/ip4/127.0.0.1/udp/20128/quic-v1"
                .parse::<Multiaddr>()
                .expect("multiaddr"),
        );
        assert_eq!(
            config.user.cryptarchia.leader.wallet.funding_pk,
            lb_key_management_system_service::keys::ZkPublicKey::zero(),
        );
    }

    #[test]
    fn deployment_override_hex_inscription_round_trips_into_genesis_inscription_bytes() {
        let (configs, genesis_tx) =
            create_general_configs(1, Some("test_override_inscription_hex"));
        let deployment_settings = e2e_deployment_settings_with_genesis_tx(genesis_tx);
        let mut config = create_validator_config(configs[0].clone(), deployment_settings);
        let mut world = CucumberWorld::default();

        // Hex input
        set_deployment_config_override(
            &mut world,
            "test-step",
            "cryptarchia.genesis_state.mantle_tx.ops.1.payload.inscription",
            "hex(70726f636573735f73746172745f6e6f6e6365)",
        )
        .expect("inscription hex override");

        apply_deployment_config_overrides(&mut config, &world.deployment_config_overrides)
            .expect("apply deployment overrides");

        let yaml = serde_yaml::to_value(&config.deployment).expect("deployment yaml");
        let inscription = get_at_path(
            &yaml,
            &[
                "cryptarchia",
                "genesis_state",
                "mantle_tx",
                "ops",
                "1",
                "payload",
                "inscription",
            ],
        )
        .expect("inscription path");

        let seq = inscription.as_sequence().expect("inscription bytes");
        let got = seq
            .iter()
            .map(|v| v.as_i64().expect("byte") as u8)
            .collect::<Vec<_>>();

        assert_eq!(got, b"process_start_nonce");

        // Human readable string input
        set_deployment_config_override(
            &mut world,
            "test-step",
            "cryptarchia.genesis_state.mantle_tx.ops.1.payload.inscription",
            "process_start_nonce",
        )
        .expect("inscription text override");

        apply_deployment_config_overrides(&mut config, &world.deployment_config_overrides)
            .expect("apply deployment overrides");

        let yaml = serde_yaml::to_value(&config.deployment).expect("deployment yaml");
        let inscription = get_at_path(
            &yaml,
            &[
                "cryptarchia",
                "genesis_state",
                "mantle_tx",
                "ops",
                "1",
                "payload",
                "inscription",
            ],
        )
        .expect("inscription path");

        let seq = inscription.as_sequence().expect("inscription bytes");
        let got = seq
            .iter()
            .map(|v| v.as_i64().expect("byte") as u8)
            .collect::<Vec<_>>();

        assert_eq!(got, b"process_start_nonce");
    }

    #[test]
    fn coerce_hex_string_to_bytes_converts_hex_string_to_byte_sequence() {
        let existing_bytes: Vec<YamlValue> = vec![
            YamlValue::from(0i64),
            YamlValue::from(1i64),
            YamlValue::from(2i64),
            YamlValue::from(3i64),
        ];
        let root = YamlValue::Mapping({
            let mut m = Mapping::new();
            m.insert(
                YamlValue::String("data".to_owned()),
                YamlValue::Sequence(existing_bytes),
            );
            m
        });

        let parsed = parse_value("test-step", "hex(deadbeef)").expect("hex override");
        let result =
            coerce_at_path(&root, &["data"], parsed, "data").expect("coerce should succeed");

        let seq = result.as_sequence().expect("result should be a sequence");
        assert_eq!(seq.len(), 4);
        assert_eq!(seq[0].as_i64(), Some(0xde));
        assert_eq!(seq[1].as_i64(), Some(0xad));
        assert_eq!(seq[2].as_i64(), Some(0xbe));
        assert_eq!(seq[3].as_i64(), Some(0xef));
    }

    #[test]
    fn coerce_seconds_converts_to_existing_duration_shape() {
        let root = YamlValue::Mapping({
            let mut m = Mapping::new();
            m.insert(
                YamlValue::String("duration".to_owned()),
                YamlValue::String("0.000000000".to_owned()),
            );
            m
        });

        let parsed = parse_value("test-step", "seconds(1.2)").expect("seconds override");
        let result = coerce_at_path(&root, &["duration"], parsed, "duration")
            .expect("coerce should succeed");

        assert_eq!(result.as_str(), Some("1.200000000"));
    }
}
