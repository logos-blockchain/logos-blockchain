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
        |key| key.escape_debug().to_string(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeOrigin {
    OldConfig,
    Extra,
}

impl Display for MergeOrigin {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OldConfig => write!(f, "old config"),
            Self::Extra => write!(f, "extra values"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeConflict {
    TypeMismatch {
        key: YamlKey,
        origin: MergeOrigin,
        source_value: YamlValue,
        destination_value: YamlValue,
    },
    KeyNotFoundInDestination {
        key: YamlKey,
        origin: MergeOrigin,
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

impl Display for MergeConflict {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypeMismatch {
                key,
                origin,
                source_value,
                destination_value,
            } => write!(
                f,
                "Type mismatch at '{key}': {origin} has {} ({}) but new config has {} ({}). Kept new value.",
                type_name(source_value),
                value_as_str(source_value),
                type_name(destination_value),
                value_as_str(destination_value),
            ),
            Self::KeyNotFoundInDestination {
                key,
                origin,
                source_value,
            } => write!(
                f,
                "Key '{key}' not found in new config. Value in {origin}: {}",
                value_as_str(source_value),
            ),
        }
    }
}

/// Merges `source` onto `destination`. Then `extra`.
///
/// - Maps are merged key by key.
/// - Lists and tagged values are replaced whole: changes nested inside them are
///   neither merged nor reported as conflicts.
///
/// # Important
///
/// Cryptographic keys are not handled specially: key IDs are merged like any
/// other value.
///
/// This means that if `destination` was generated from a different keystore
/// than `source`, the result will reference keys it doesn't have.
///
/// Generate `destination` from the source's keystore with
/// [`migrate::run`](super::migrate::run) to avoid this.
pub fn merge(
    source: YamlValue,
    destination: &mut YamlValue,
    extra: Option<YamlValue>,
    flags: &MergeFlags,
) -> Vec<MergeConflict> {
    let key = YamlKey::root();
    let mut conflicts = Vec::new();

    let merge_source_conflicts = merge_value(
        key.clone(),
        source,
        destination,
        MergeOrigin::OldConfig,
        flags.source_insert_missing,
    );
    conflicts.extend(merge_source_conflicts);

    if let Some(extra) = extra {
        let merge_extra_conflicts = merge_value(
            key,
            extra,
            destination,
            MergeOrigin::Extra,
            flags.extra_insert_missing,
        );
        conflicts.extend(merge_extra_conflicts);
    }

    conflicts
}

pub fn run(
    source_path: &Path,
    destination_path: &Path,
    extra: Option<YamlValue>,
    flags: &MergeFlags,
) -> Result<Vec<MergeConflict>> {
    let source_yaml = std::fs::read_to_string(source_path)?;
    let source: YamlValue = serde_yaml::from_str(&source_yaml)?;

    let destination_yaml = std::fs::read_to_string(destination_path)?;
    let mut destination: YamlValue = serde_yaml::from_str(&destination_yaml)?;

    let conflicts = merge(source, &mut destination, extra, flags);

    let destination_yaml = serde_yaml::to_string(&destination)?;
    std::fs::write(destination_path, destination_yaml)?;

    Ok(conflicts)
}

fn merge_value(
    source_key: YamlKey,
    source: YamlValue,
    destination: &mut YamlValue,
    origin: MergeOrigin,
    insert_if_missing: bool,
) -> Vec<MergeConflict> {
    match (source, destination) {
        (YamlValue::Null, destination) => *destination = YamlValue::Null,
        (source, destination @ YamlValue::Null) => *destination = source,
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
                origin,
                insert_if_missing,
            );
        }
        (YamlValue::Tagged(source_value), YamlValue::Tagged(destination_value)) => {
            *destination_value = source_value;
        }
        (source_value, destination_value) => {
            let mismatch = MergeConflict::TypeMismatch {
                key: source_key,
                origin,
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
    origin: MergeOrigin,
    insert_if_missing: bool,
) -> Vec<MergeConflict> {
    let mut conflicts = Vec::new();
    for (key, value) in source_mapping {
        let mapping_key = destination_mapping.get_mut(&key);
        let source_mapping_key = source_key.clone().push(key.clone());

        if let Some(destination_mapping_value) = mapping_key {
            let merge_value_conflicts = merge_value(
                source_mapping_key,
                value,
                destination_mapping_value,
                origin,
                insert_if_missing,
            );
            conflicts.extend(merge_value_conflicts);
        } else if insert_if_missing {
            destination_mapping.insert(key, value);
        } else {
            let not_found = MergeConflict::KeyNotFoundInDestination {
                key: source_mapping_key,
                origin,
                source_value: value,
            };
            conflicts.push(not_found);
        }
    }
    conflicts
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
    fn yaml_key_escapes_invisible_characters_in_string_steps() {
        let key = key(&["a\nb", "c\0d"]);

        assert_eq!(key.to_string(), r"a\nb.c\0d");
    }

    #[test]
    fn type_mismatch_displays_key_types_and_values() {
        let conflict = MergeConflict::TypeMismatch {
            key: key(&["a", "b"]),
            origin: MergeOrigin::OldConfig,
            source_value: yaml("{ c: [1, text] }"),
            destination_value: yaml("1"),
        };

        assert_eq!(
            conflict.to_string(),
            r#"Type mismatch at 'a.b': old config has map ({"c":[1,"text"]}) but new config has number (1). Kept new value."#
        );
    }

    #[test]
    fn not_found_displays_key_and_source_value() {
        let conflict = MergeConflict::KeyNotFoundInDestination {
            key: key(&["a", "b"]),
            origin: MergeOrigin::OldConfig,
            source_value: yaml("text"),
        };

        assert_eq!(
            conflict.to_string(),
            r#"Key 'a.b' not found in new config. Value in old config: "text""#
        );
    }

    #[test]
    fn conflict_displays_extra_origin() {
        let conflict = MergeConflict::KeyNotFoundInDestination {
            key: key(&["a"]),
            origin: MergeOrigin::Extra,
            source_value: yaml("1"),
        };

        assert_eq!(
            conflict.to_string(),
            "Key 'a' not found in new config. Value in extra values: 1"
        );
    }

    #[test]
    fn extra_value_overrides_source_value() {
        let source = yaml("a: 2");
        let mut destination = yaml("a: 1");
        let extra = Some(yaml("a: 3"));

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: 3"));
    }

    #[test]
    fn source_insert_flag_only_inserts_source_keys() {
        let source = yaml("b: 2");
        let mut destination = yaml("a: 1");
        let extra = Some(yaml("c: 3"));

        let conflicts = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::KeyNotFoundInDestination {
                key: key(&["c"]),
                origin: MergeOrigin::Extra,
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

        let conflicts = merge(source, &mut destination, extra, &EXTRA_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::KeyNotFoundInDestination {
                key: key(&["b"]),
                origin: MergeOrigin::OldConfig,
                source_value: yaml("2"),
            }]
        );
        assert_eq!(destination, yaml("{ a: 1, c: 3 }"));
    }

    #[test]
    fn run_writes_merged_destination_file_and_returns_conflicts() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.yaml");
        let destination_path = temp_dir.path().join("destination.yaml");
        std::fs::write(&source_path, "{ a: 2, b: 2 }").unwrap();
        std::fs::write(&destination_path, "{ a: 1, c: 1 }").unwrap();
        let extra = Some(yaml("c: 3"));

        let conflicts = run(&source_path, &destination_path, extra, &NO_INSERT).unwrap();

        assert_eq!(
            conflicts,
            vec![MergeConflict::KeyNotFoundInDestination {
                key: key(&["b"]),
                origin: MergeOrigin::OldConfig,
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

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: null"));
    }

    #[test]
    fn source_value_replaces_destination_null() {
        let source = yaml("a: 30");
        let mut destination = yaml("a: null");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: 30"));
    }

    #[test]
    fn source_null_replaces_destination_value() {
        let source = yaml("a: null");
        let mut destination = yaml("a: 30");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: null"));
    }

    #[test]
    fn bool_is_replaced() {
        let source = yaml("a: true");
        let mut destination = yaml("a: false");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: true"));
    }

    #[test]
    fn number_is_replaced() {
        let source = yaml("a: 10");
        let mut destination = yaml("a: 1");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: 10"));
    }

    #[test]
    fn string_is_replaced() {
        let source = yaml("a: new");
        let mut destination = yaml("a: old");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: new"));
    }

    #[test]
    fn sequence_is_replaced_whole() {
        let source = yaml("a: [9]");
        let mut destination = yaml("a: [1, 2, 3]");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: [9]"));
    }

    #[test]
    fn mapping_merges_recursively_and_keeps_unmatched_destination_keys() {
        let source = yaml("a: { b: { c: 10 } }");
        let mut destination = yaml("a: { b: { c: 1, d: 2 }, e: 3 }");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: { b: { c: 10, d: 2 }, e: 3 }"));
    }

    #[test]
    fn tagged_is_replaced_including_tag() {
        let source = yaml("a: !x 1");
        let mut destination = yaml("a: !y 2");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: !x 1"));
    }

    #[test]
    fn type_mismatch_keeps_destination_value_and_reports_path() {
        let source = yaml("a: { b: text }");
        let mut destination = yaml("a: { b: 1 }");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::TypeMismatch {
                key: key(&["a", "b"]),
                origin: MergeOrigin::OldConfig,
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

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::TypeMismatch {
                key: YamlKey::root(),
                origin: MergeOrigin::OldConfig,
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

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::TypeMismatch {
                key: key(&["a"]),
                origin: MergeOrigin::OldConfig,
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

        let conflicts = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert!(conflicts.is_empty());
        assert_eq!(destination, yaml("a: { b: { c: 1 } }"));
    }

    #[test]
    fn insert_flag_does_not_change_existing_keys_handling() {
        let source = yaml("{ a: 10, b: text, c: 3 }");
        let mut destination = yaml("{ a: 1, b: 1 }");
        let extra = None;

        let conflicts = merge(source, &mut destination, extra, &SOURCE_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::TypeMismatch {
                key: key(&["b"]),
                origin: MergeOrigin::OldConfig,
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

        let conflicts = merge(source, &mut destination, extra, &NO_INSERT);

        assert_eq!(
            conflicts,
            vec![MergeConflict::KeyNotFoundInDestination {
                key: key(&["a", "b"]),
                origin: MergeOrigin::OldConfig,
                source_value: yaml("1"),
            }]
        );
        assert_eq!(destination, yaml("a: {}"));
    }
}
