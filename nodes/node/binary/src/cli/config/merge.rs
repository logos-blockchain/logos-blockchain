use std::{
    fmt::{Display, Formatter},
    path::Path,
};

use color_eyre::eyre::Result;
use serde_yaml::{Mapping, Value as YamlValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlKey {
    keys: Vec<YamlValue>,
}

fn key_as_str(key: &YamlValue) -> String {
    key.as_str().map_or_else(
        || {
            serde_yaml::to_string(key)
                .unwrap_or_default()
                .trim_end()
                .to_owned()
        },
        ToOwned::to_owned,
    )
}

impl YamlKey {
    #[must_use]
    pub const fn root() -> Self {
        Self { keys: Vec::new() }
    }

    #[must_use]
    pub fn push(mut self, step: YamlValue) -> Self {
        self.keys.push(step);
        self
    }
}

impl Display for YamlKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let full_key = self
            .keys
            .iter()
            .map(key_as_str)
            .collect::<Vec<_>>()
            .join(".");
        write!(f, "{full_key}")
    }
}

pub struct MergeFlags {
    pub source_insert_missing: bool,
    pub extra_insert_missing: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeError {
    TypeMismatch {
        key: YamlKey,
        source_value: YamlValue,
        destination_value: YamlValue,
    },
    KeyNotFoundInDestination {
        key: YamlKey,
        source_value: YamlValue,
    },
}

fn value_as_str(value: &YamlValue) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

const fn type_name(value: &YamlValue) -> &'static str {
    match value {
        YamlValue::Null => "null",
        YamlValue::Bool(_) => "boolean",
        YamlValue::Number(_) => "number",
        YamlValue::String(_) => "string",
        YamlValue::Sequence(_) => "list",
        YamlValue::Mapping(_) => "map",
        YamlValue::Tagged(_) => "tagged value",
    }
}

impl Display for MergeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeMismatch {
                key,
                source_value,
                destination_value,
            } => write!(
                f,
                "Type mismatch at '{key}'. Old config has {} ({}) but new config has {} ({}). Kept new value.",
                type_name(source_value),
                value_as_str(source_value),
                type_name(destination_value),
                value_as_str(destination_value),
            ),
            Self::KeyNotFoundInDestination { key, source_value } => write!(
                f,
                "Key '{key}' not found in new config. Value in old config: {}",
                value_as_str(source_value),
            ),
        }
    }
}

pub fn merge(
    source: YamlValue,
    destination: &mut YamlValue,
    extra: Option<YamlValue>,
    flags: &MergeFlags,
) -> Vec<MergeError> {
    let key = YamlKey::root();
    let mut errors = Vec::new();

    let merge_source_errors = merge_value(
        key.clone(),
        source,
        destination,
        flags.source_insert_missing,
    );
    errors.extend(merge_source_errors);

    if let Some(extra) = extra {
        let merge_extra_errors = merge_value(key, extra, destination, flags.extra_insert_missing);
        errors.extend(merge_extra_errors);
    }

    errors
}

pub fn run(
    source_path: &Path,
    destination_path: &Path,
    extra: Option<YamlValue>,
    flags: &MergeFlags,
) -> Result<Vec<MergeError>> {
    let source_yaml = std::fs::read_to_string(source_path)?;
    let source: YamlValue = serde_yaml::from_str(&source_yaml)?;

    let destination_yaml = std::fs::read_to_string(destination_path)?;
    let mut destination: YamlValue = serde_yaml::from_str(&destination_yaml)?;

    let errors = merge(source, &mut destination, extra, flags);

    let destination_yaml = serde_yaml::to_string(&destination)?;
    std::fs::write(destination_path, destination_yaml)?;

    Ok(errors)
}

fn merge_value(
    source_key: YamlKey,
    source: YamlValue,
    destination: &mut YamlValue,
    insert_if_missing: bool,
) -> Vec<MergeError> {
    match (source, destination) {
        (YamlValue::Null, YamlValue::Null) => {}
        (YamlValue::Bool(source_value), YamlValue::Bool(destination_value)) => {
            *destination_value = source_value;
        }
        (YamlValue::Number(source_value), YamlValue::Number(destination_value)) => {
            *destination_value = source_value;
        }
        (YamlValue::String(source_value), YamlValue::String(destination_value)) => {
            *destination_value = source_value;
        }
        (YamlValue::Sequence(source_value), YamlValue::Sequence(destination_value)) => {
            *destination_value = source_value;
        }
        (YamlValue::Mapping(source_mapping), YamlValue::Mapping(destination_mapping)) => {
            return merge_mapping(
                &source_key,
                source_mapping,
                destination_mapping,
                insert_if_missing,
            );
        }
        (YamlValue::Tagged(source_value), YamlValue::Tagged(destination_value)) => {
            *destination_value = source_value;
        }
        (source_value, destination_value) => {
            let mismatch = MergeError::TypeMismatch {
                key: source_key,
                source_value,
                destination_value: destination_value.clone(),
            };
            return vec![mismatch];
        }
    }

    Vec::new()
}

fn merge_mapping(
    source_key: &YamlKey,
    source_mapping: Mapping,
    destination_mapping: &mut Mapping,
    insert_if_missing: bool,
) -> Vec<MergeError> {
    let mut errors = Vec::new();
    for (key, value) in source_mapping {
        let mapping_key = destination_mapping.get_mut(&key);
        let source_mapping_key = source_key.clone().push(key.clone());

        if let Some(destination_mapping_value) = mapping_key {
            let merge_value_errors = merge_value(
                source_mapping_key,
                value,
                destination_mapping_value,
                insert_if_missing,
            );
            errors.extend(merge_value_errors);
        } else if insert_if_missing {
            destination_mapping.insert(key, value);
        } else {
            let not_found = MergeError::KeyNotFoundInDestination {
                key: source_mapping_key,
                source_value: value,
            };
            errors.push(not_found);
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const NO_INSERT: MergeFlags = MergeFlags {
        source_insert_missing: false,
        extra_insert_missing: false,
    };

    const SOURCE_INSERT: MergeFlags = MergeFlags {
        source_insert_missing: true,
        extra_insert_missing: false,
    };

    const EXTRA_INSERT: MergeFlags = MergeFlags {
        source_insert_missing: false,
        extra_insert_missing: true,
    };

    fn yaml(text: &str) -> YamlValue {
        serde_yaml::from_str(text).unwrap()
    }

    fn key(steps: &[&str]) -> YamlKey {
        steps
            .iter()
            .fold(YamlKey::root(), |key, step| key.push((*step).into()))
    }

    #[test]
    fn yaml_key_root_displays_empty() {
        let key = YamlKey::root();

        assert_eq!(key.to_string(), "");
    }

    #[test]
    fn yaml_key_displays_string_steps_joined_by_dots() {
        let key = key(&["a", "b", "c"]);

        assert_eq!(key.to_string(), "a.b.c");
    }

    #[test]
    fn yaml_key_displays_non_string_steps_as_yaml() {
        let key = YamlKey::root()
            .push(7.into())
            .push(true.into())
            .push(YamlValue::Null);

        assert_eq!(key.to_string(), "7.true.null");
    }

    #[test]
    fn type_mismatch_displays_key_types_and_values() {
        let error = MergeError::TypeMismatch {
            key: key(&["a", "b"]),
            source_value: yaml("{ c: [1, text] }"),
            destination_value: yaml("1"),
        };

        assert_eq!(
            error.to_string(),
            r#"Type mismatch at 'a.b'. Old config has map ({"c":[1,"text"]}) but new config has number (1). Kept new value."#
        );
    }

    #[test]
    fn not_found_displays_key_and_source_value() {
        let error = MergeError::KeyNotFoundInDestination {
            key: key(&["a", "b"]),
            source_value: yaml("text"),
        };

        assert_eq!(
            error.to_string(),
            r#"Key 'a.b' not found in new config. Value in old config: "text""#
        );
    }

    #[test]
    fn extra_value_overrides_source_value() {
        let source = yaml("a: 2");
        let mut destination = yaml("a: 1");
        let extra = Some(yaml("a: 3"));

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: 3"));
    }

    #[test]
    fn source_insert_flag_only_inserts_source_keys() {
        let source = yaml("b: 2");
        let mut destination = yaml("a: 1");
        let extra = Some(yaml("c: 3"));

        let errors = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::KeyNotFoundInDestination {
                key: key(&["c"]),
                source_value: yaml("3"),
            }]
        );
        assert_eq!(destination, yaml("{ a: 1, b: 2 }"));
    }

    #[test]
    fn extra_insert_flag_only_inserts_extra_keys() {
        let source = yaml("b: 2");
        let mut destination = yaml("a: 1");
        let extra = Some(yaml("c: 3"));

        let errors = merge(source, &mut destination, extra, &EXTRA_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::KeyNotFoundInDestination {
                key: key(&["b"]),
                source_value: yaml("2"),
            }]
        );
        assert_eq!(destination, yaml("{ a: 1, c: 3 }"));
    }

    #[test]
    fn run_writes_merged_destination_file_and_returns_errors() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&source_path, "{ a: 2, b: 2 }").unwrap();
        std::fs::write(&destination_path, "{ a: 1, c: 1 }").unwrap();
        let extra = Some(yaml("c: 3"));

        let errors = run(&source_path, &destination_path, extra, &NO_INSERT).unwrap();

        assert_eq!(
            errors,
            vec![MergeError::KeyNotFoundInDestination {
                key: key(&["b"]),
                source_value: yaml("2"),
            }]
        );
        let destination = yaml(&std::fs::read_to_string(&destination_path).unwrap());
        assert_eq!(destination, yaml("{ a: 2, c: 3 }"));
    }

    #[test]
    fn run_fails_and_keeps_destination_file_when_source_file_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&destination_path, "a: 1").unwrap();

        let result = run(&source_path, &destination_path, None, &NO_INSERT);

        assert!(result.is_err());
        let destination = std::fs::read_to_string(&destination_path).unwrap();
        assert_eq!(destination, "a: 1");
    }

    #[test]
    fn null_is_kept() {
        let source = yaml("a: null");
        let mut destination = yaml("a: null");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: null"));
    }

    #[test]
    fn bool_is_replaced() {
        let source = yaml("a: true");
        let mut destination = yaml("a: false");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: true"));
    }

    #[test]
    fn number_is_replaced() {
        let source = yaml("a: 10");
        let mut destination = yaml("a: 1");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: 10"));
    }

    #[test]
    fn string_is_replaced() {
        let source = yaml("a: new");
        let mut destination = yaml("a: old");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: new"));
    }

    #[test]
    fn sequence_is_replaced_whole() {
        let source = yaml("a: [9]");
        let mut destination = yaml("a: [1, 2, 3]");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: [9]"));
    }

    #[test]
    fn mapping_merges_recursively_and_keeps_unmatched_destination_keys() {
        let source = yaml("a: { b: { c: 10 } }");
        let mut destination = yaml("a: { b: { c: 1, d: 2 }, e: 3 }");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: { b: { c: 10, d: 2 }, e: 3 }"));
    }

    #[test]
    fn tagged_is_replaced_including_tag() {
        let source = yaml("a: !x 1");
        let mut destination = yaml("a: !y 2");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: !x 1"));
    }

    #[test]
    fn type_mismatch_keeps_destination_value_and_reports_path() {
        let source = yaml("a: { b: text }");
        let mut destination = yaml("a: { b: 1 }");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::TypeMismatch {
                key: key(&["a", "b"]),
                source_value: yaml("text"),
                destination_value: yaml("1"),
            }]
        );
        assert_eq!(destination, yaml("a: { b: 1 }"));
    }

    #[test]
    fn type_mismatch_at_root_reports_root_path() {
        let source = yaml("[1]");
        let mut destination = yaml("a: 1");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::TypeMismatch {
                key: YamlKey::root(),
                source_value: yaml("[1]"),
                destination_value: yaml("a: 1"),
            }]
        );
        assert_eq!(destination, yaml("a: 1"));
    }

    #[test]
    fn type_mismatch_does_not_stop_sibling_keys() {
        let source = yaml("{ a: text, b: 2 }");
        let mut destination = yaml("{ a: 1, b: 1 }");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::TypeMismatch {
                key: key(&["a"]),
                source_value: yaml("text"),
                destination_value: yaml("1"),
            }]
        );
        assert_eq!(destination, yaml("{ a: 1, b: 2 }"));
    }

    #[test]
    fn missing_key_is_inserted_with_its_subtree_when_flag_set() {
        let source = yaml("a: { b: { c: 1 } }");
        let mut destination = yaml("a: {}");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert!(errors.is_empty());
        assert_eq!(destination, yaml("a: { b: { c: 1 } }"));
    }

    #[test]
    fn insert_flag_does_not_change_existing_keys_handling() {
        let source = yaml("{ a: 10, b: text, c: 3 }");
        let mut destination = yaml("{ a: 1, b: 1 }");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::TypeMismatch {
                key: key(&["b"]),
                source_value: yaml("text"),
                destination_value: yaml("1"),
            }]
        );
        assert_eq!(destination, yaml("{ a: 10, b: 1, c: 3 }"));
    }

    #[test]
    fn missing_key_is_reported_with_full_path_and_not_inserted() {
        let source = yaml("a: { b: 1 }");
        let mut destination = yaml("a: {}");
        let extra = None;

        let errors = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            errors,
            vec![MergeError::KeyNotFoundInDestination {
                key: key(&["a", "b"]),
                source_value: yaml("1"),
            }]
        );
        assert_eq!(destination, yaml("a: {}"));
    }
}
