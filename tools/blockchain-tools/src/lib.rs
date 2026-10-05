pub mod genesis;
use serde_yaml::Value;

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

/// Set `value` at the dot-separated `path` inside `root`.
///
/// The container a segment lands on decides how it is read: a sequence takes
/// the segment as an index, a mapping as a key. A segment spelling out one of
/// a mapping's integer keys names that key, as `0` does in `eras.0.time`,
/// where eras are keyed by first epoch; any other segment is a string key.
/// Missing mapping keys, and any parents they need, are created.
///
/// # Errors
///
/// Returns an error string when a segment is empty, does not index an existing
/// element of a sequence, or descends into a scalar.
pub fn set_at_path(root: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let mut current = root;
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(format!("empty segment in path '{path}'"));
        }
        if current.is_null() {
            *current = Value::Mapping(serde_yaml::Mapping::new());
        }
        current = match current {
            Value::Sequence(sequence) => {
                let length = sequence.len();
                segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| sequence.get_mut(index))
                    .ok_or_else(|| {
                        format!(
                            "'{segment}' in '{path}' is not an index into a sequence of \
                             {length} elements"
                        )
                    })?
            }
            Value::Mapping(mapping) => {
                let key = mapping_key(mapping, segment);
                mapping.entry(key).or_insert(Value::Null)
            }
            _ => return Err(format!("'{path}' descends into a scalar at '{segment}'")),
        };
    }
    *current = value;
    Ok(())
}

/// The key `segment` names in `mapping`: the integer key it spells out when
/// the mapping holds one, and otherwise the string itself.
fn mapping_key(mapping: &serde_yaml::Mapping, segment: &str) -> Value {
    segment
        .parse::<u64>()
        .ok()
        .map(Value::from)
        .filter(|key| mapping.contains_key(key))
        .unwrap_or_else(|| Value::String(segment.to_owned()))
}

/// Apply a dot-notation `"some.nested.key=value"` override to `root`, as
/// [`set_at_path`] does. The value portion is parsed as YAML so integers,
/// booleans, quoted strings, etc. are typed correctly.
///
/// # Errors
///
/// Returns an error string when no `=` separator is found, when the value
/// portion is not valid YAML, or when [`set_at_path`] rejects the path.
pub fn apply_dotted_kv(root: &mut Value, s: &str) -> Result<(), String> {
    let (path, raw_value) = s
        .split_once('=')
        .ok_or_else(|| format!("missing '=' separator in override: {s}"))?;

    let value: Value = serde_yaml::from_str(raw_value)
        .map_err(|e| format!("invalid YAML value '{raw_value}': {e}"))?;

    set_at_path(root, path, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Value {
        serde_yaml::from_str(text).unwrap()
    }

    #[test]
    fn dotted_kv_single_key() {
        let mut root = Value::Null;
        apply_dotted_kv(&mut root, "foo=bar").unwrap();
        assert_eq!(root, yaml("foo: bar"));
    }

    #[test]
    fn dotted_kv_nested() {
        let mut root = yaml("a:\n  x: 1");
        apply_dotted_kv(&mut root, "a.b.c=42").unwrap();
        assert_eq!(root, yaml("a:\n  x: 1\n  b:\n    c: 42"));
    }

    #[test]
    fn dotted_kv_missing_eq() {
        assert!(apply_dotted_kv(&mut Value::Null, "no-separator").is_err());
    }

    #[test]
    fn a_numeric_segment_indexes_a_sequence() {
        let mut root = yaml("peers:\n- host: a\n  port: 1\n- host: b\n  port: 2");
        apply_dotted_kv(&mut root, "peers.1.port=30").unwrap();
        assert_eq!(
            root,
            yaml("peers:\n- host: a\n  port: 1\n- host: b\n  port: 30")
        );
    }

    #[test]
    fn a_numeric_segment_names_an_existing_integer_key() {
        let mut root = yaml("eras:\n  0:\n    k: 1\n    f: 2");
        apply_dotted_kv(&mut root, "eras.0.k=30").unwrap();
        assert_eq!(root, yaml("eras:\n  0:\n    k: 30\n    f: 2"));
    }

    #[test]
    fn a_numeric_segment_is_a_key_in_a_mapping() {
        let mut root = yaml("a:\n  x: 1");
        set_at_path(&mut root, "a.0", yaml("2")).unwrap();
        assert_eq!(root, yaml("a:\n  x: 1\n  '0': 2"));
    }

    #[test]
    fn an_index_past_the_end_of_a_sequence_is_rejected() {
        let mut root = yaml("eras:\n- first_epoch: 0");
        assert!(set_at_path(&mut root, "eras.1.first_epoch", yaml("5")).is_err());
        assert!(set_at_path(&mut root, "eras.last", yaml("5")).is_err());
    }

    #[test]
    fn descending_into_a_scalar_is_rejected() {
        let mut root = yaml("a: 1");
        assert!(set_at_path(&mut root, "a.b", yaml("2")).is_err());
    }

    #[test]
    fn an_empty_segment_is_rejected() {
        assert!(set_at_path(&mut Value::Null, "a..b", yaml("2")).is_err());
    }

    #[test]
    fn overwrite_yaml_merges_nested() {
        let base: Value = serde_yaml::from_str("a:\n  x: 1\n  y: 2").unwrap();
        let patch: Value = serde_yaml::from_str("a:\n  y: 99\n  z: 3").unwrap();
        let result = overwrite_yaml(base, patch);
        let expected: Value = serde_yaml::from_str("a:\n  x: 1\n  y: 99\n  z: 3").unwrap();
        assert_eq!(result, expected);
    }
}
