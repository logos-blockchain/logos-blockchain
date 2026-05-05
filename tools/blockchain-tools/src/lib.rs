pub mod distribution;
use serde_yml::Value;

/// Deep-merge `overwrite` into `input`. Mappings are merged recursively;
/// any other type is replaced wholesale by the overwrite value.
#[must_use]
pub fn overwrite_yaml(input: Value, overwrite: Value) -> Value {
    match (input, overwrite) {
        (Value::Mapping(mut input_map), Value::Mapping(overwrite_map)) => {
            for (key, overwrite_value) in overwrite_map {
                input_map
                    .entry(key)
                    .and_modify(|input_value| {
                        *input_value = overwrite_yaml(input_value.clone(), overwrite_value.clone());
                    })
                    .or_insert(overwrite_value);
            }
            Value::Mapping(input_map)
        }
        (_, overwrite) => overwrite,
    }
}

#[expect(
    clippy::too_long_first_doc_paragraph,
    reason = "Necessary documentation"
)]
/// Convert a dot-notation `"some.nested.key=value"` string into a nested
/// [`serde_yml::Value`] mapping. The value portion is parsed as YAML so
/// integers, booleans, quoted strings, etc. are typed correctly.
///
/// # Errors
///
/// Returns an error string when no `=` separator is found or when the value
/// portion is not valid YAML.
pub fn value_from_dotted_kv(s: &str) -> Result<Value, String> {
    let (key_path, raw_value) = s
        .split_once('=')
        .ok_or_else(|| format!("missing '=' separator in override: {s}"))?;

    let leaf: Value = serde_yml::from_str(raw_value)
        .map_err(|e| format!("invalid YAML value '{raw_value}': {e}"))?;

    // Wrap the leaf in nested mappings, innermost key first (right-to-left fold).
    let nested = key_path.split('.').rev().fold(leaf, |acc, key| {
        let mut map = serde_yml::Mapping::new();
        map.insert(Value::String(key.to_owned()), acc);
        Value::Mapping(map)
    });

    Ok(nested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_kv_single_key() {
        let v = value_from_dotted_kv("foo=bar").unwrap();
        let expected: Value = serde_yml::from_str("foo: bar").unwrap();
        assert_eq!(v, expected);
    }

    #[test]
    fn dotted_kv_nested() {
        let v = value_from_dotted_kv("a.b.c=42").unwrap();
        let expected: Value = serde_yml::from_str("a:\n  b:\n    c: 42").unwrap();
        assert_eq!(v, expected);
    }

    #[test]
    fn dotted_kv_missing_eq() {
        assert!(value_from_dotted_kv("no-separator").is_err());
    }

    #[test]
    fn overwrite_yaml_merges_nested() {
        let base: Value = serde_yml::from_str("a:\n  x: 1\n  y: 2").unwrap();
        let patch: Value = serde_yml::from_str("a:\n  y: 99\n  z: 3").unwrap();
        let result = overwrite_yaml(base, patch);
        let expected: Value = serde_yml::from_str("a:\n  x: 1\n  y: 99\n  z: 3").unwrap();
        assert_eq!(result, expected);
    }
}
