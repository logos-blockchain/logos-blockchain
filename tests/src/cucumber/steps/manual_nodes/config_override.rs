use lb_node::config::RunConfig;
use serde_yaml::{Mapping, Value as YamlValue};

use crate::cucumber::{
    error::{StepError, StepResult},
    world::{CucumberWorld, UserConfigOverride},
};

/// Publishes a user config override to the world, to be applied to nodes when
/// they are created.
pub fn set_user_config_override(
    world: &mut CucumberWorld,
    step: &str,
    raw_path: &str,
    raw_value: &str,
) -> StepResult {
    let path = normalize_user_config_path(step, raw_path)?;
    let value = parse_user_config_step_value(step, raw_value)?;

    if let Some(existing) = world
        .user_config_overrides
        .iter_mut()
        .find(|override_item| override_item.path == path)
    {
        existing.value = value;
        return Ok(());
    }

    world
        .user_config_overrides
        .push(UserConfigOverride { path, value });
    Ok(())
}

/// Applies a user config override to the runtime config.
pub fn apply_user_config_overrides(
    config: &mut RunConfig,
    user_config_overrides: &[UserConfigOverride],
) -> Result<(), StepError> {
    if user_config_overrides.is_empty() {
        return Ok(());
    }

    let mut user_config =
        serde_yaml::to_value(&config.user).map_err(|source| StepError::LogicalError {
            message: format!("Failed to serialize node user config for patching: {source}"),
        })?;

    for override_item in user_config_overrides {
        let path = override_item.path.split('.').collect::<Vec<_>>();
        set_yaml_value_at_path(
            &mut user_config,
            &path,
            override_item.value.clone(),
            &override_item.path,
        )?;
    }

    config.user =
        serde_yaml::from_value(user_config).map_err(|source| StepError::InvalidArgument {
            message: format!(
                "Invalid user config override combination. Resulting config could not be deserialized: {source}"
            ),
        })?;

    Ok(())
}

fn normalize_user_config_path(step: &str, raw_path: &str) -> Result<String, StepError> {
    let path = raw_path.trim();
    if path.is_empty() {
        return Err(StepError::InvalidArgument {
            message: format!("Step `{step}` error: user config path cannot be empty"),
        });
    }

    let mut normalized = String::with_capacity(path.len());
    for segment in path.split('.').map(str::trim) {
        if segment.is_empty() {
            return Err(StepError::InvalidArgument {
                message: format!(
                    "Step `{step}` error: user config path '{path}' has an empty segment"
                ),
            });
        }

        if !normalized.is_empty() {
            normalized.push('.');
        }
        normalized.push_str(segment);
    }

    Ok(normalized)
}

fn parse_user_config_step_value(step: &str, raw_value: &str) -> Result<YamlValue, StepError> {
    let value = raw_value.trim();
    serde_yaml::from_str::<YamlValue>(value).map_err(|source| StepError::InvalidArgument {
        message: format!(
            "Step `{step}` error: user config value '{value}' is not valid YAML: {source}"
        ),
    })
}

fn set_yaml_value_at_path(
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
    let is_last = path.len() == 1;

    if let Ok(index) = segment.parse::<usize>() {
        if current.is_null() {
            *current = YamlValue::Sequence(Vec::new());
        }

        let sequence =
            current
                .as_sequence_mut()
                .ok_or_else(|| StepError::InvalidArgument {
                    message: format!(
                        "Invalid user config override path '{full_path}': segment '{segment}' expects a YAML sequence"
                    ),
                })?;

        if sequence.len() <= index {
            sequence.resize(index + 1, YamlValue::Null);
        }

        if is_last {
            sequence[index] = value;
            return Ok(());
        }

        return set_yaml_value_at_path(&mut sequence[index], &path[1..], value, full_path);
    }

    if current.is_null() {
        *current = YamlValue::Mapping(Mapping::new());
    }

    let mapping = current
        .as_mapping_mut()
        .ok_or_else(|| StepError::InvalidArgument {
            message: format!(
                "Invalid user config override path '{full_path}': segment '{segment}' expects a YAML mapping"
            ),
        })?;

    let key = YamlValue::String(segment.to_owned());
    if is_last {
        mapping.insert(key, value);
        return Ok(());
    }

    let next_is_sequence = path[1].parse::<usize>().is_ok();
    let next_default = if next_is_sequence {
        YamlValue::Sequence(Vec::new())
    } else {
        YamlValue::Mapping(Mapping::new())
    };
    let child = mapping.entry(key).or_insert(next_default);
    if child.is_null() {
        *child = if next_is_sequence {
            YamlValue::Sequence(Vec::new())
        } else {
            YamlValue::Mapping(Mapping::new())
        };
    }

    set_yaml_value_at_path(child, &path[1..], value, full_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_user_config_path_rejects_empty_segments() {
        let path = "network..backend";
        assert!(normalize_user_config_path("test-step", path).is_err());
    }

    #[test]
    fn set_yaml_value_at_path_supports_nested_mapping() {
        let mut root = YamlValue::Mapping(Mapping::new());
        set_yaml_value_at_path(
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
    fn set_yaml_value_at_path_supports_sequence_indices() {
        let mut root = YamlValue::Mapping(Mapping::new());
        set_yaml_value_at_path(
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
}
