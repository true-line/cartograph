//! Project a bounded response schema onto the JSON-schema subset that the
//! Anthropic Messages API compiles for `output_config.format` (#1709).
//!
//! The API rejects some keywords, such as numeric and length constraints, with
//! a 400. Those keywords are dropped from the wire copy only. The host still
//! validates every response against the complete original schema, so admission
//! is unchanged. Any keyword outside the known-compilable allowlist fails
//! closed before sending. It is never forwarded or silently dropped.
use serde_json::{Map, Value};

/// Keywords dropped from the wire copy. The host re-validates all of them.
const RELAXED: &[&str] = &[
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "maxItems",
    "uniqueItems",
];

/// Schema-valued keywords whose value is itself a schema.
const SCHEMA: &[&str] = &["items"];
/// Keywords whose value is a list of schemas.
const SCHEMA_LIST: &[&str] = &["anyOf", "allOf"];
/// Keywords whose value maps names to schemas.
const SCHEMA_MAP: &[&str] = &["properties", "$defs", "definitions"];
/// Keywords whose value passes through verbatim.
const VERBATIM: &[&str] = &[
    "type",
    "required",
    "enum",
    "const",
    "$ref",
    "default",
    "description",
    "title",
    "format",
    "pattern",
];

/// Returns `None` when the schema uses anything the API cannot compile.
pub(crate) fn project(schema: &str) -> Option<Value> {
    node(&serde_json::from_str(schema).ok()?)
}

fn node(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mut out = Map::new();
    for (key, value) in object {
        let key = key.as_str();
        if RELAXED.contains(&key) {
            continue;
        }
        let projected = match key {
            // Only 0 and 1 compile. A larger minimum stays host-enforced.
            "minItems" => match value.as_u64()? {
                0 | 1 => value.clone(),
                _ => continue,
            },
            // The only accepted form; anything else cannot be represented.
            "additionalProperties" if value == &Value::Bool(false) => value.clone(),
            // Exclusive alternatives become `anyOf` on the wire. The host's
            // full-schema validation still enforces exactly-one.
            "oneOf" => {
                if out.insert("anyOf".into(), list(value)?).is_some() {
                    return None;
                }
                continue;
            }
            // `enum` compiles only for scalar members. Structured members (for
            // example acquisition-choice actions) become exact `const` branches.
            "enum" if !value.as_array()?.iter().all(scalar) => {
                let branches = value
                    .as_array()?
                    .iter()
                    .map(|member| serde_json::json!({ "const": member }))
                    .collect();
                if out.insert("anyOf".into(), Value::Array(branches)).is_some() {
                    return None;
                }
                continue;
            }
            _ if SCHEMA.contains(&key) => node(value)?,
            _ if SCHEMA_LIST.contains(&key) => list(value)?,
            _ if SCHEMA_MAP.contains(&key) => {
                let mut map = Map::new();
                for (name, schema) in value.as_object()? {
                    map.insert(name.clone(), node(schema)?);
                }
                Value::Object(map)
            }
            _ if VERBATIM.contains(&key) => value.clone(),
            _ => return None,
        };
        if out.insert(key.into(), projected).is_some() {
            return None;
        }
    }
    Some(Value::Object(out))
}

fn scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn list(value: &Value) -> Option<Value> {
    value
        .as_array()?
        .iter()
        .map(node)
        .collect::<Option<Vec<_>>>()
        .map(Value::Array)
}

#[cfg(test)]
mod tests {
    use super::project;
    use serde_json::json;

    // AC-0185 / #1709: constraints the API rejects are relaxed on the wire only.
    #[test]
    fn relaxes_rejected_constraints_and_keeps_structure() {
        let wire = project(
            &json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["a", "b"],
                "properties": {
                    "a": {"type": "string", "minLength": 1, "maxLength": 64, "pattern": "^x"},
                    "b": {"type": "array", "minItems": 3, "maxItems": 9, "uniqueItems": true,
                          "items": {"type": "integer", "minimum": 0, "maximum": 5}},
                    "minLength": {"type": "boolean"}
                },
                "$defs": {"c": {"oneOf": [{"const": "x"}, {"const": "y"}]}}
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            wire,
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["a", "b"],
                "properties": {
                    "a": {"type": "string", "pattern": "^x"},
                    "b": {"type": "array", "items": {"type": "integer"}},
                    "minLength": {"type": "boolean"}
                },
                "$defs": {"c": {"anyOf": [{"const": "x"}, {"const": "y"}]}}
            })
        );
        assert_eq!(
            project(r#"{"type":"array","minItems":1}"#).unwrap(),
            json!({"type": "array", "minItems": 1})
        );
    }

    // AC-0185 / #1709: structured enum members (acquisition actions) become exact const
    // branches; scalar enums are unchanged.
    #[test]
    fn structured_enum_members_become_const_branches() {
        let action = json!({"type": "read_evidence", "fact": {"id": "f1"}, "index": 0});
        assert_eq!(
            project(&json!({"enum": [action.clone(), "x"]}).to_string()).unwrap(),
            json!({"anyOf": [{"const": action}, {"const": "x"}]})
        );
        assert_eq!(
            project(r#"{"enum":["a",1,null]}"#).unwrap(),
            json!({"enum": ["a", 1, null]})
        );
        assert_eq!(project(r#"{"enum":[{}],"anyOf":[]}"#), None);
    }

    // AC-0185 / #1709: unknown or unrepresentable forms fail closed; nothing is guessed.
    #[test]
    fn unsupported_forms_fail_closed() {
        for schema in [
            r#"{"type":"object","additionalProperties":true}"#,
            r#"{"type":"object","additionalProperties":{"type":"string"}}"#,
            r#"{"not":{"type":"string"}}"#,
            r#"{"if":{"type":"string"},"then":{}}"#,
            r#"{"patternProperties":{"^x":{}}}"#,
            r##"{"$ref":"#/$defs/a","oneOf":[],"anyOf":[]}"##,
            r#"{"minItems":-1}"#,
            r#"[]"#,
            r#"{"type":"object","properties":{"a":true}}"#,
        ] {
            assert_eq!(project(schema), None, "{schema}");
        }
    }

    // AC-0185 / #1709: the shipped analyst schemas project without loss of structure.
    #[test]
    fn analyst_schemas_project() {
        for schema in [
            include_str!("../../../../agents/src/investigation/data/analyst-v13-choice.json"),
            include_str!("../../../../agents/src/investigation/data/analyst-v13-finish-only.json"),
        ] {
            let wire = project(schema).expect("analyst schema must project");
            let text = wire.to_string();
            for rejected in [
                "minLength",
                "maxLength",
                "\"minimum\"",
                "\"maximum\"",
                "maxItems",
                "uniqueItems",
                "oneOf",
            ] {
                assert!(!text.contains(rejected), "{rejected}");
            }
        }
    }
}
