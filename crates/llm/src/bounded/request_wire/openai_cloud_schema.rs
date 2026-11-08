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
use std::collections::HashMap;

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
    Walk {
        root: &wire,
        totals: Totals::default(),
        resolved: HashMap::new(),
    }
    .node(&wire)?;
    Some((wire, wrapped))
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

/// One structural pass over the wire schema. Each node, and each referenced
/// definition, is visited and counted once. A reference reuses its target's
/// memoized depth, so work stays linear in the schema size. Every limit is
/// enforced as soon as it is exceeded.
struct Walk<'a> {
    root: &'a Value,
    totals: Totals,
    /// Memoized depth per referenced or root-defined schema, keyed by address.
    resolved: HashMap<*const Value, usize>,
}

impl<'a> Walk<'a> {
    fn within_limits(&self) -> Option<()> {
        (self.totals.properties <= MAX_PROPERTIES
            && self.totals.enum_values <= MAX_ENUM_VALUES
            && self.totals.string_bytes <= MAX_STRING_BYTES)
            .then_some(())
    }

    /// The depth of a definition, checked and counted on first use only. The
    /// projection has already proved references local and acyclic, so the
    /// recursion terminates.
    fn resolve(&mut self, definition: &'a Value) -> Option<usize> {
        let key: *const Value = definition;
        if let Some(depth) = self.resolved.get(&key) {
            return Some(*depth);
        }
        let depth = self.node(definition)?;
        self.resolved.insert(key, depth);
        Some(depth)
    }

    /// Checks strict-mode rules for `node` and returns its nesting depth.
    fn node(&mut self, node: &'a Value) -> Option<usize> {
        let object = node.as_object()?;
        // Strict mode has no `allOf`, and a reference carries no sibling
        // constraints. Neither is forwarded unchecked.
        if object.contains_key("allOf") {
            return None;
        }
        if let Some(reference) = object.get("$ref") {
            if object.len() != 1 {
                return None;
            }
            let root = self.root;
            return self.resolve(root.pointer(reference.as_str()?.strip_prefix('#')?)?);
        }
        for literal in object
            .get("enum")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.totals.enum_values += 1;
            self.totals.string_bytes += literal.as_str().map_or(0, str::len);
        }
        match object.get("const") {
            Some(Value::String(literal)) => self.totals.string_bytes += literal.len(),
            // Structured constants count by serialized size, which errs toward refusing.
            Some(literal @ (Value::Object(_) | Value::Array(_))) => {
                self.totals.string_bytes += literal.to_string().len()
            }
            _ => {}
        }
        self.within_limits()?;
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
                self.totals.properties += 1;
                self.totals.string_bytes += name.len();
                self.within_limits()?;
                depth = depth.max(1 + self.node(schema)?);
            }
        } else if object.get("type") == Some(&json!("object")) {
            // An open object without declared properties has no strict form.
            return None;
        }
        if let Some(items) = object.get("items") {
            depth = depth.max(1 + self.node(items)?);
        }
        for branch in object
            .get("anyOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            depth = depth.max(self.node(branch)?);
        }
        if std::ptr::eq(node, self.root) {
            // Unreferenced definitions are checked too, each exactly once.
            for key in ["$defs", "definitions"] {
                for (name, definition) in object
                    .get(key)
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    self.totals.string_bytes += name.len();
                    self.within_limits()?;
                    self.resolve(definition)?;
                }
            }
        } else if object.contains_key("$defs") || object.contains_key("definitions") {
            // References resolve only against root definitions.
            return None;
        }
        (depth <= MAX_DEPTH).then_some(depth)
    }
}

#[cfg(test)]
mod tests {
    use super::{project, unwrap};
    use serde_json::json;

    // AC-0567 / #1709: an anyOf root travels wrapped with its definitions kept at
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

    // AC-0567 / #1709: strict-mode object rules and published limits fail closed.
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
        // An unreferenced definition is depth-checked too.
        let unreferenced = json!({"type": "object", "additionalProperties": false,
            "required": [], "properties": {}, "$defs": {"deep": deep}});
        assert_eq!(project(&unreferenced.to_string()), None);
        for schema in [
            // `allOf` branches and `$ref` siblings are never forwarded unchecked.
            json!({"type": "object", "additionalProperties": false, "required": [],
                "properties": {}, "allOf": [{"type": "object",
                "properties": {"x": {"type": "string"}}, "required": ["x"]}]}),
            json!({"type": "object", "additionalProperties": false, "required": ["a"],
                "properties": {"a": {"$ref": "#/$defs/s", "type": "object"}},
                "$defs": {"s": {"type": "string"}}}),
        ] {
            assert_eq!(project(&schema.to_string()), None, "{schema}");
        }
        let many: Vec<_> = (0..1001).map(|value| json!(value)).collect();
        assert_eq!(
            project(&closed(json!({"a": {"enum": many}}), json!(["a"]))),
            None
        );
    }

    // AC-0567 / #1709: a reference DAG whose full expansion is exponential is
    // checked once per definition, and each definition counts once.
    #[test]
    fn reference_dag_is_checked_linearly() {
        let mut definitions = serde_json::Map::new();
        definitions.insert("d0".into(), json!({"type": "string"}));
        for level in 1..=64 {
            let below = format!("#/$defs/d{}", level - 1);
            definitions.insert(
                format!("d{level}"),
                json!({"anyOf": [{"$ref": below}, {"$ref": below}]}),
            );
        }
        let schema = json!({"anyOf": [{"$ref": "#/$defs/d64"}], "$defs": definitions});
        // Exponential expansion (2^64 visits) would never finish.
        assert!(project(&schema.to_string()).is_some());
    }

    // AC-0567 / #1709: the shipped analyst schemas satisfy strict mode once wrapped.
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
