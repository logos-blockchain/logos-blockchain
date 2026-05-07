use std::path::{Path, PathBuf};

use serde_yaml::Value;

#[derive(Debug, thiserror::Error)]
pub enum YamlIncludeError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serde(#[from] serde_yaml::Error),
    #[error("!include error: {0}")]
    InvalidInclude(String),
}

/// Recursively resolves `!include <path>` tags in a YAML value.
///
/// Paths are resolved relative to `base_dir`. Nested includes are supported
/// (each included file's own includes resolve relative to that file's
/// directory).
pub fn resolve_includes(value: Value, base_dir: &Path) -> Result<Value, YamlIncludeError> {
    match value {
        Value::Tagged(tagged) if tagged.tag == "include" => {
            let path_str = match &tagged.value {
                Value::String(s) => s.clone(),
                other => {
                    return Err(YamlIncludeError::InvalidInclude(format!(
                        "!include requires a string path, got: {other:?}"
                    )));
                }
            };
            let resolved_path = if Path::new(&path_str).is_absolute() {
                PathBuf::from(&path_str)
            } else {
                base_dir.join(&path_str)
            };
            let contents = std::fs::read_to_string(&resolved_path)?;
            let included: Value = serde_yaml::from_str(&contents)?;
            let new_base = resolved_path.parent().unwrap_or(base_dir).to_path_buf();
            resolve_includes(included, &new_base)
        }
        Value::Mapping(map) => {
            let resolved = map
                .into_iter()
                .map(|(k, v)| Ok((k, resolve_includes(v, base_dir)?)))
                .collect::<Result<serde_yaml::Mapping, YamlIncludeError>>()?;
            Ok(Value::Mapping(resolved))
        }
        Value::Sequence(seq) => {
            let resolved = seq
                .into_iter()
                .map(|v| resolve_includes(v, base_dir))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Sequence(resolved))
        }
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_yaml::{
        Value,
        value::{Tag, TaggedValue},
    };
    use tempfile::TempDir;

    use super::{YamlIncludeError, resolve_includes};

    fn include_tag(path: &str) -> Value {
        Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new("include"),
            value: Value::String(path.into()),
        }))
    }

    fn mapping(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        let m = pairs
            .into_iter()
            .map(|(k, v)| (Value::String(k.into()), v))
            .collect();
        Value::Mapping(m)
    }

    #[test]
    fn passthrough_when_no_includes() {
        let input = mapping([
            ("a", Value::Number(1.into())),
            ("b", Value::Bool(true)),
            ("c", Value::String("hello".into())),
        ]);
        let result = resolve_includes(input.clone(), Path::new(".")).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn include_substitutes_file_contents() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("sub.yaml"), "value: 42\n").unwrap();

        let input = mapping([("key", include_tag("sub.yaml"))]);
        let result = resolve_includes(input, dir.path()).unwrap();

        let expected = mapping([("key", mapping([("value", Value::Number(42.into()))]))]);
        assert_eq!(result, expected);
    }

    #[test]
    fn include_in_sequence() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.yaml"), "1\n").unwrap();
        fs::write(dir.path().join("b.yaml"), "2\n").unwrap();

        let input = Value::Sequence(vec![include_tag("a.yaml"), include_tag("b.yaml")]);
        let result = resolve_includes(input, dir.path()).unwrap();

        assert_eq!(
            result,
            Value::Sequence(vec![Value::Number(1.into()), Value::Number(2.into())])
        );
    }

    #[test]
    fn nested_include() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("c.yaml"), "leaf: true\n").unwrap();
        fs::write(dir.path().join("b.yaml"), "nested: !include c.yaml\n").unwrap();
        fs::write(dir.path().join("a.yaml"), "top: !include b.yaml\n").unwrap();

        let input: Value = serde_yaml::from_str("root: !include a.yaml\n").unwrap();
        let result = resolve_includes(input, dir.path()).unwrap();

        let expected = mapping([(
            "root",
            mapping([(
                "top",
                mapping([("nested", mapping([("leaf", Value::Bool(true))]))]),
            )]),
        )]);
        assert_eq!(result, expected);
    }

    #[test]
    fn include_resolves_relative_to_included_file_directory() {
        let dir = TempDir::new().unwrap();
        let sub_dir = dir.path().join("sub");
        fs::create_dir_all(&sub_dir).unwrap();

        fs::write(sub_dir.join("leaf.yaml"), "x: 1\n").unwrap();
        fs::write(sub_dir.join("inner.yaml"), "data: !include leaf.yaml\n").unwrap();

        let input = mapping([("result", include_tag("sub/inner.yaml"))]);
        let result = resolve_includes(input, dir.path()).unwrap();

        let expected = mapping([(
            "result",
            mapping([("data", mapping([("x", Value::Number(1.into()))]))]),
        )]);
        assert_eq!(result, expected);
    }

    #[test]
    fn absolute_path_include() {
        let dir = TempDir::new().unwrap();
        let abs_path = dir.path().join("abs.yaml");
        fs::write(&abs_path, "absolute: true\n").unwrap();

        let input = mapping([("k", include_tag(abs_path.to_str().unwrap()))]);
        let result = resolve_includes(input, Path::new("/nonexistent")).unwrap();

        let expected = mapping([("k", mapping([("absolute", Value::Bool(true))]))]);
        assert_eq!(result, expected);
    }

    #[test]
    fn missing_file_returns_io_error() {
        let input = mapping([("k", include_tag("does_not_exist.yaml"))]);
        let err = resolve_includes(input, Path::new(".")).unwrap_err();
        assert!(matches!(err, YamlIncludeError::Io(_)));
    }

    #[test]
    fn non_string_include_value_returns_invalid_include_error() {
        let input = Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new("include"),
            value: Value::Number(42.into()),
        }));
        let err = resolve_includes(input, Path::new(".")).unwrap_err();
        assert!(matches!(err, YamlIncludeError::InvalidInclude(_)));
    }
}
