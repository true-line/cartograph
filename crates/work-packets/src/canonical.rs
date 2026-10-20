use serde_json::Value;
use std::io::Write;

/// Independent bounds on canonical output and traversal of an admitted value.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalLimits {
    /// Maximum UTF-8 output bytes, including JSON escaping and punctuation.
    pub bytes: usize,
    /// Maximum nesting depth; the root has depth zero.
    pub depth: usize,
    /// Maximum number of values, including container values.
    pub values: usize,
    /// Maximum keys in any single object, checked before sorting keys.
    pub object_keys: usize,
}

/// Canonical encoding failed without returning a partial representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalError {
    /// Output or structural traversal exceeded a declared limit.
    #[error("canonical JSON exceeds its declared bounds")]
    LimitExceeded,
}

/// Encode JSON with recursively sorted object keys and unchanged array order.
///
/// Strings and numbers use serde_json's normal representation. This does not
/// normalize sets: the typed caller must normalize only declared unordered
/// fields. No cloned JSON tree or unbounded escaped intermediate is produced.
pub fn canonical_json(value: &Value, limits: CanonicalLimits) -> Result<Vec<u8>, CanonicalError> {
    let mut output = Output {
        bytes: Vec::new(),
        maximum: limits.bytes,
    };
    let mut remaining = limits.values;
    encode(value, 0, limits, &mut remaining, &mut output)?;
    Ok(output.bytes)
}

struct Output {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("canonical JSON byte limit"));
        }
        // Grow geometrically within the ceiling, avoiding a reallocation for
        // every punctuation byte without requesting unadmitted capacity.
        let required = self.bytes.len() + bytes.len();
        if required > self.bytes.capacity() {
            let capacity = required.max(
                self.bytes
                    .capacity()
                    .saturating_mul(2)
                    .max(64)
                    .min(self.maximum),
            );
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| std::io::Error::other("canonical JSON allocation failed"))?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode(
    value: &Value,
    depth: usize,
    limits: CanonicalLimits,
    remaining: &mut usize,
    output: &mut Output,
) -> Result<(), CanonicalError> {
    let exceeded = || CanonicalError::LimitExceeded;
    if depth > limits.depth || *remaining == 0 {
        return Err(exceeded());
    }
    *remaining -= 1;
    match value {
        Value::Object(object) => {
            if object.len() > limits.object_keys || object.len() > *remaining {
                return Err(exceeded());
            }
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            output.write_all(b"{").map_err(|_| exceeded())?;
            for (index, (key, child)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.write_all(b",").map_err(|_| exceeded())?;
                }
                serde_json::to_writer(&mut *output, key).map_err(|_| exceeded())?;
                output.write_all(b":").map_err(|_| exceeded())?;
                encode(child, depth + 1, limits, remaining, output)?;
            }
            output.write_all(b"}").map_err(|_| exceeded())?;
        }
        Value::Array(array) => {
            if array.len() > *remaining {
                return Err(exceeded());
            }
            output.write_all(b"[").map_err(|_| exceeded())?;
            for (index, child) in array.iter().enumerate() {
                if index != 0 {
                    output.write_all(b",").map_err(|_| exceeded())?;
                }
                encode(child, depth + 1, limits, remaining, output)?;
            }
            output.write_all(b"]").map_err(|_| exceeded())?;
        }
        scalar => serde_json::to_writer(output, scalar).map_err(|_| exceeded())?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits(bytes: usize) -> CanonicalLimits {
        CanonicalLimits {
            bytes,
            depth: 40,
            values: 65_536,
            object_keys: 64,
        }
    }

    #[test]
    fn recursively_orders_keys_but_preserves_arrays_and_exact_strings() {
        let first: Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1},3],"a":"é\n"}"#).unwrap();
        let reordered: Value =
            serde_json::from_str(r#"{"a":"é\n","z":[{"a":1,"b":2},3]}"#).unwrap();
        let expected = "{\"a\":\"é\\n\",\"z\":[{\"a\":1,\"b\":2},3]}".as_bytes();
        assert_eq!(canonical_json(&first, limits(1024)).unwrap(), expected);
        assert_eq!(
            canonical_json(&first, limits(1024)),
            canonical_json(&reordered, limits(1024))
        );
        assert_ne!(
            canonical_json(&json!([1, 2]), limits(20)),
            canonical_json(&json!([2, 1]), limits(20))
        );
    }

    #[test]
    fn charges_escaped_bytes_at_the_exact_output_boundary() {
        let value = json!({"x": "\u{0000}é\"\\"});
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(canonical_json(&value, limits(bytes.len())).unwrap(), bytes);
        assert_eq!(
            canonical_json(&value, limits(bytes.len() - 1)),
            Err(CanonicalError::LimitExceeded)
        );
        assert_eq!(
            canonical_json(&value, limits(bytes.len() + 1)).unwrap(),
            bytes
        );
        let large = Value::String("\u{0000}".repeat(100_000));
        assert_eq!(
            canonical_json(&large, limits(64)),
            Err(CanonicalError::LimitExceeded)
        );
    }

    #[test]
    fn structural_limits_include_containers_and_check_before_key_sorting() {
        let value = json!({"x": [1, 2]});
        let exact = CanonicalLimits {
            bytes: 100,
            depth: 2,
            values: 4,
            object_keys: 1,
        };
        assert!(canonical_json(&value, exact).is_ok());
        for changed in [
            CanonicalLimits { depth: 1, ..exact },
            CanonicalLimits { values: 3, ..exact },
            CanonicalLimits {
                object_keys: 0,
                ..exact
            },
        ] {
            assert_eq!(
                canonical_json(&value, changed),
                Err(CanonicalError::LimitExceeded)
            );
        }
        assert!(
            canonical_json(
                &json!(null),
                CanonicalLimits {
                    bytes: 4,
                    depth: 0,
                    values: 1,
                    object_keys: 0
                }
            )
            .is_ok()
        );
    }
}
