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
///
/// The API rejects `$defs` alongside `anyOf` (a 400 seen by the owner's hosted
/// preflight on every analyst schema), so the wire copy carries no definitions:
/// each local reference is replaced by its projected definition. References are
/// proven acyclic first, so inlining terminates, and a node budget bounds the
/// expansion of shared references.
pub(crate) fn project(schema: &str) -> Option<Value> {
    let wire = relaxed(schema)?;
    let mut budget = INLINE_NODE_BUDGET;
    let mut inlined = inline(&wire, &wire, true, &mut budget)?;
    if let Value::Object(map) = &mut inlined {
        map.remove("$defs");
        map.remove("definitions");
    }
    Some(inlined)
}

/// The relaxed projection with root definitions and local `$ref`s kept. The
/// OpenAI strict projection starts from this form.
pub(super) fn relaxed(schema: &str) -> Option<Value> {
    let wire = node(&serde_json::from_str(schema).ok()?)?;
    references_resolve(&wire).then_some(wire)
}

/// Upper bound on JSON nodes in an inlined wire schema. The largest shipped
/// analyst schema needs a small fraction of it; a schema whose shared
/// references expand past it fails closed instead of growing without bound.
const INLINE_NODE_BUDGET: usize = 20_000;

/// Replaces every `$ref` with a copy of its (already projected, acyclic) root
/// definition. A reference with sibling keywords, or definitions anywhere but
/// the root, fail closed: neither occurs in the shipped schemas and merging them
/// would be a guess.
fn inline(value: &Value, root: &Value, at_root: bool, budget: &mut usize) -> Option<Value> {
    *budget = budget.checked_sub(1)?;
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref") {
                if map.len() != 1 {
                    return None;
                }
                let (definitions, name) =
                    reference.as_str()?.strip_prefix("#/")?.split_once('/')?;
                return inline(root.get(definitions)?.get(name)?, root, false, budget);
            }
            let mut out = Map::new();
            for (key, child) in map {
                let projected = match key.as_str() {
                    "$defs" | "definitions" if at_root => continue,
                    "$defs" | "definitions" => return None,
                    // Literal payloads are data, never schemas: copied verbatim.
                    "const" | "enum" | "default" | "required" | "type" => child.clone(),
                    "properties" => Value::Object(
                        child
                            .as_object()?
                            .iter()
                            .map(|(name, schema)| {
                                Some((name.clone(), inline(schema, root, false, budget)?))
                            })
                            .collect::<Option<Map<_, _>>>()?,
                    ),
                    _ => inline(child, root, false, budget)?,
                };
                out.insert(key.clone(), projected);
            }
            Some(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .map(|item| inline(item, root, false, budget))
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        other => Some(other.clone()),
    }
}

/// The API accepts only local, non-recursive references. Every `$ref` must name
/// an existing `#/$defs/<name>` or `#/definitions/<name>` entry of the root, and
/// following references from any definition must never revisit it.
fn references_resolve(root: &Value) -> bool {
    let target = |reference: &Value| -> Option<(&'static str, String)> {
        let reference = reference.as_str()?;
        let (map, name) = ["$defs", "definitions"].into_iter().find_map(|map| {
            Some((
                map,
                reference
                    .strip_prefix("#/")?
                    .strip_prefix(map)?
                    .strip_prefix('/')?,
            ))
        })?;
        // Escaped or nested pointers are not resolved here; fail closed.
        if name.is_empty() || name.contains(['/', '~', '%']) {
            return None;
        }
        root.get(map)?.get(name)?;
        Some((map, name.to_owned()))
    };
    let mut references = Vec::new();
    collect(root, &mut references);
    let Some(edges_of_root) = references
        .iter()
        .map(|r| target(r))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let edges = |definition: &Value| -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        collect(definition, &mut found);
        found.iter().filter_map(|r| target(r)).collect()
    };
    // Depth-first cycle search over the definition graph.
    fn acyclic(
        node: (&'static str, String),
        root: &Value,
        edges: &dyn Fn(&Value) -> Vec<(&'static str, String)>,
        path: &mut Vec<(&'static str, String)>,
        done: &mut Vec<(&'static str, String)>,
    ) -> bool {
        if done.contains(&node) {
            return true;
        }
        if path.contains(&node) {
            return false;
        }
        path.push(node.clone());
        let next = edges(&root[node.0][node.1.as_str()]);
        let ok = next
            .into_iter()
            .all(|n| acyclic(n, root, edges, path, done));
        path.pop();
        done.push(node);
        ok
    }
    let mut done = Vec::new();
    edges_of_root
        .into_iter()
        .all(|n| acyclic(n, root, &edges, &mut Vec::new(), &mut done))
}

fn collect<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "$ref" {
                    out.push(child);
                } else if !matches!(key.as_str(), "const" | "enum" | "default") {
                    collect(child, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|child| collect(child, out)),
        _ => {}
    }
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

    // AC-0568 / #1709: constraints the API rejects are relaxed on the wire only.
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
                }
            })
        );
        assert_eq!(
            project(r#"{"type":"array","minItems":1}"#).unwrap(),
            json!({"type": "array", "minItems": 1})
        );
    }

    // AC-0568 / #1709: structured enum members (acquisition actions) become exact const
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

    // AC-0568 / #1709: unknown or unrepresentable forms fail closed; nothing is guessed.
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
            r#"{"$ref":"https://example.com/schema"}"#,
            r##"{"$ref":"#/$defs/missing","$defs":{"a":{}}}"##,
            r##"{"$ref":"#/$defs/a~1b","$defs":{"a/b":{}}}"##,
            r##"{"$ref":"#"}"##,
            r##"{"$ref":"#/$defs/a","$defs":{"a":{"items":{"$ref":"#/$defs/a"}}}}"##,
            r##"{"$ref":"#/$defs/a","$defs":{"a":{"$ref":"#/definitions/b"}},"definitions":{"b":{"anyOf":[{"$ref":"#/$defs/a"}]}}}"##,
        ] {
            assert_eq!(project(schema), None, "{schema}");
        }
    }

    // AC-0568 / #1709: local acyclic references, including shared targets, pass.
    #[test]
    fn local_acyclic_references_project() {
        let schema = r##"{"anyOf":[{"$ref":"#/$defs/a"},{"$ref":"#/$defs/b"}],
            "$defs":{"a":{"$ref":"#/$defs/c"},"b":{"$ref":"#/$defs/c"},
                     "c":{"const":{"$ref":"#/$defs/nowhere"}}}}"##;
        assert_eq!(
            project(schema).unwrap(),
            json!({"anyOf": [
                {"const": {"$ref": "#/$defs/nowhere"}},
                {"const": {"$ref": "#/$defs/nowhere"}}
            ]})
        );
    }

    // AC-0568 / #1709: the API rejects `$defs` with `anyOf` (owner hosted
    // preflight, 400 on all eight analyst schemas). The wire copy inlines every
    // reference and carries no definitions; forms that would need a guess fail
    // closed.
    #[test]
    fn references_are_inlined_without_definitions() {
        let schema = r##"{"anyOf":[{"$ref":"#/$defs/finding"},{"type":"null"}],
            "$defs":{"finding":{"type":"object","additionalProperties":false,"required":["id"],
                     "properties":{"id":{"type":"string","maxLength":8},"kind":{"$ref":"#/definitions/kind"}}}},
            "definitions":{"kind":{"enum":["a","b"]}}}"##;
        assert_eq!(
            project(schema).unwrap(),
            json!({"anyOf": [
                {"type": "object", "additionalProperties": false, "required": ["id"],
                 "properties": {"id": {"type": "string"}, "kind": {"enum": ["a", "b"]}}},
                {"type": "null"}
            ]})
        );
        for schema in [
            r##"{"anyOf":[{"$ref":"#/$defs/a","description":"x"}],"$defs":{"a":{}}}"##,
            r##"{"anyOf":[{"type":"object","$defs":{"a":{}}}]}"##,
        ] {
            assert_eq!(project(schema), None, "{schema}");
        }
        // Each level references the next twice: 2^20 leaves exceeds the budget,
        // while a shallow chain of the same shape stays well inside it.
        let chain = |depth: usize| {
            let mut defs = serde_json::Map::new();
            for level in 0..depth {
                defs.insert(
                    format!("d{level}"),
                    json!({"anyOf": [{"$ref": format!("#/$defs/d{}", level + 1)},
                                     {"$ref": format!("#/$defs/d{}", level + 1)}]}),
                );
            }
            defs.insert(format!("d{depth}"), json!({"type": "null"}));
            json!({"anyOf": [{"$ref": "#/$defs/d0"}], "$defs": defs}).to_string()
        };
        assert!(project(&chain(6)).is_some());
        assert_eq!(project(&chain(20)), None);
        assert!(super::relaxed(&chain(20)).is_some());
    }

    // AC-0568 / #1709: the shipped analyst schemas project without loss of structure.
    #[test]
    fn analyst_schemas_project() {
        for schema in [
            include_str!("../../../../agents/src/investigation/data/analyst-v13-choice.json"),
            include_str!("../../../../agents/src/investigation/data/analyst-v13-finish-only.json"),
        ] {
            let wire = project(schema).expect("analyst schema must project");
            let text = wire.to_string();
            assert!(wire.get("$defs").is_none() && wire.get("definitions").is_none());
            for rejected in [
                "\"$defs\"",
                "\"$ref\":\"#/",
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
