//! Project a bounded response schema onto OpenAI's strict structured-output
//! subset (#1709). It starts from the Anthropic projection: rejected
//! constraints relaxed, `oneOf` as `anyOf`, structured enum members as `const`
//! branches, and local acyclic references only. Then it enforces the strict-mode
//! rules and published limits before sending.
//!
//! Strict mode requires an object root, but every analyst schema is a root
//! `anyOf`. Such a root travels wrapped as the single required property
//! [`WRAPPER`]. [`unwrap`] returns the model's exact inner bytes, and the host
//! validates those against the complete original schema. Admission is
//! unchanged.
use serde_json::{Map, Value, json};

/// The only property of a wrapped wire root.
pub(crate) const WRAPPER: &str = "response";

const MAX_PROPERTIES: usize = 5000;
const MAX_DEPTH: usize = 10;
const MAX_ENUM_VALUES: usize = 1000;
const MAX_STRING_BYTES: usize = 120_000;

/// The wire schema and whether its root is wrapped. `None` when strict mode
/// cannot represent the schema.
pub(crate) fn project(schema: &str) -> Option<(Value, bool)> {
    let mut wire = super::anthropic_schema::project(schema)?;
    let wrapped = !(wire.get("type") == Some(&json!("object")) && wire.get("anyOf").is_none());
    if wrapped {
        let root = wire.as_object_mut()?;
        let mut outer = Map::new();
        // Definitions stay at the root so `#/$defs/...` pointers still resolve.
        for key in ["$defs", "definitions"] {
            if let Some(definitions) = root.remove(key) {
                outer.insert(key.into(), definitions);
            }
        }
        outer.insert("type".into(), json!("object"));
        outer.insert(
            "properties".into(),
            json!({ WRAPPER: Value::Object(std::mem::take(root)) }),
        );
        outer.insert("required".into(), json!([WRAPPER]));
        outer.insert("additionalProperties".into(), json!(false));
        wire = Value::Object(outer);
    }
    let mut totals = Totals::default();
    let depth = strict(&wire, &wire, &mut totals)?;
    (depth <= MAX_DEPTH
        && totals.properties <= MAX_PROPERTIES
        && totals.enum_values <= MAX_ENUM_VALUES
        && totals.string_bytes <= MAX_STRING_BYTES)
        .then_some((wire, wrapped))
}

/// The model's exact inner bytes from a wrapped response. Anything other than
/// one object holding exactly the wrapper property is `None`.
pub(crate) fn unwrap(text: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wrapped<'a> {
        #[serde(borrow)]
        response: &'a serde_json::value::RawValue,
    }
    // A derived struct also accepts the sequence form; only an object counts.
    if !text.trim_start().starts_with('{') {
        return None;
    }
    let wrapped: Wrapped<'_> = serde_json::from_str(text).ok()?;
    Some(wrapped.response.get().to_owned())
}

#[derive(Default)]
struct Totals {
    properties: usize,
    enum_values: usize,
    string_bytes: usize,
}

/// Checks strict-mode object rules for `node` and returns its nesting depth.
/// References resolve against `root`. The projection has already proved them
/// local and acyclic, so following them terminates.
fn strict(node: &Value, root: &Value, totals: &mut Totals) -> Option<usize> {
    let object = node.as_object()?;
    if let Some(reference) = object.get("$ref") {
        let pointer = reference.as_str()?.strip_prefix('#')?;
        return strict(root.pointer(pointer)?, root, totals);
    }
    for literal in object
        .get("enum")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        totals.enum_values += 1;
        totals.string_bytes += literal.as_str().map_or(0, str::len);
    }
    if let Some(Value::String(literal)) = object.get("const") {
        totals.string_bytes += literal.len();
    }
    let mut depth = 0;
    if let Some(properties) = object.get("properties") {
        let properties = properties.as_object()?;
        // Every object is closed, and every property is required.
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)?
            .iter()
            .map(Value::as_str)
            .collect::<Option<_>>()?;
        if object.get("additionalProperties") != Some(&Value::Bool(false))
            || required.len() != properties.len()
            || !properties
                .keys()
                .all(|name| required.contains(&name.as_str()))
        {
            return None;
        }
        for (name, schema) in properties {
            totals.properties += 1;
            totals.string_bytes += name.len();
            depth = depth.max(1 + strict(schema, root, totals)?);
        }
    } else if object.get("type") == Some(&json!("object")) {
        // An open object without declared properties has no strict form.
        return None;
    }
    if let Some(items) = object.get("items") {
        depth = depth.max(1 + strict(items, root, totals)?);
    }
    for branch in object
        .get("anyOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        depth = depth.max(strict(branch, root, totals)?);
    }
    for key in ["$defs", "definitions"] {
        // Unreferenced definitions are still checked; repeated visits only
        // over-count the totals, which errs toward refusing.
        for (name, definition) in object
            .get(key)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            totals.string_bytes += name.len();
            strict(definition, root, totals)?;
        }
    }
    Some(depth)
}

#[cfg(test)]
mod tests {
    use super::{project, unwrap};
    use serde_json::json;

    // AC-0185 / #1709: an anyOf root travels wrapped with its definitions kept at
    // the root; the exact inner bytes come back out.
    #[test]
    fn any_of_root_is_wrapped_and_unwrapped_exactly() {
        let schema = json!({
            "anyOf": [{"$ref": "#/$defs/a"}],
            "$defs": {"a": {"type": "object", "additionalProperties": false,
                "required": ["k"], "properties": {"k": {"type": "string", "maxLength": 3}}}}
        });
        let (wire, wrapped) = project(&schema.to_string()).unwrap();
        assert!(wrapped);
        assert_eq!(
            wire,
            json!({
                "type": "object", "additionalProperties": false, "required": ["response"],
                "properties": {"response": {"anyOf": [{"$ref": "#/$defs/a"}]}},
                "$defs": {"a": {"type": "object", "additionalProperties": false,
                    "required": ["k"], "properties": {"k": {"type": "string"}}}}
            })
        );
        assert_eq!(
            unwrap(r#"{"response": {"k" : "x",  "z":1.50}}"#).unwrap(),
            r#"{"k" : "x",  "z":1.50}"#
        );
        for rejected in [
            r#"{"response":{},"other":1}"#,
            r#"{"response":{},"response":{}}"#,
            r#"{}"#,
            r#"[{"response":{}}]"#,
        ] {
            assert_eq!(unwrap(rejected), None, "{rejected}");
        }
        let object = json!({"type": "object", "additionalProperties": false,
            "required": [], "properties": {}});
        assert_eq!(project(&object.to_string()), Some((object, false)));
    }

    // AC-0185 / #1709: strict-mode object rules and published limits fail closed.
    #[test]
    fn strict_rules_and_limits_fail_closed() {
        let closed = |properties: serde_json::Value, required: serde_json::Value| {
            json!({"type": "object", "additionalProperties": false,
                "properties": properties, "required": required})
            .to_string()
        };
        for schema in [
            json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]})
                .to_string(),
            closed(
                json!({"a": {"type": "string"}, "b": {"type": "string"}}),
                json!(["a"]),
            ),
            closed(json!({"a": {"type": "object"}}), json!(["a"])),
            closed(json!({"a": {"type": "string"}}), json!(["a", "b"])),
        ] {
            assert_eq!(project(&schema), None, "{schema}");
        }
        let mut deep = json!({"type": "string"});
        for _ in 0..11 {
            deep = json!({"type": "object", "additionalProperties": false,
                "properties": {"a": deep}, "required": ["a"]});
        }
        assert_eq!(project(&deep.to_string()), None);
        let many: Vec<_> = (0..1001).map(|value| json!(value)).collect();
        assert_eq!(
            project(&closed(json!({"a": {"enum": many}}), json!(["a"]))),
            None
        );
    }

    // AC-0185 / #1709: the shipped analyst schemas satisfy strict mode once wrapped.
    #[test]
    fn analyst_schemas_project_strictly() {
        for schema in [
            include_str!("../../../../agents/src/investigation/data/analyst-v13-choice.json"),
            include_str!("../../../../agents/src/investigation/data/analyst-v13-finish-only.json"),
        ] {
            let (_, wrapped) = project(schema).expect("analyst schema must project strictly");
            assert!(wrapped);
        }
    }
}
