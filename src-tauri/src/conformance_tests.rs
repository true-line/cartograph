//! Coverage-matrix conformance (AC-0253): every supported or partial cell's
//! fixture runs through the full T0 pipeline and must yield its expected facts.

use ingest::matrix::COVERAGE_MATRIX;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

const REPO: &str = "local/p";

fn identity(expr: &adapters_fw::events::IdentityExpr) -> &str {
    use adapters_fw::events::IdentityExpr::*;
    match expr {
        Literal(s) | EnvRef(s) | Computed(s) => s,
    }
}

/// `want` is a subset of `got`: every key in `want` is present and equal.
fn subset(want: &Value, got: &Value) -> bool {
    match (want, got) {
        (Value::Object(w), Value::Object(g)) => w
            .iter()
            .all(|(k, v)| g.get(k).is_some_and(|gv| subset(v, gv))),
        _ => want == got,
    }
}

/// Required and optional keys of each expectation entry, by section.
const SECTIONS: &[(&str, &[&str], &[&str])] = &[
    ("nodes", &["label", "id"], &["props"]),
    ("edges", &["label", "src", "dst"], &[]),
    ("event_sites", &["kind", "role", "identity"], &[]),
    ("fetch_sites", &["method", "url"], &[]),
];

/// An `expect.json` must assert something real: known sections only, each a
/// nonempty array of entries with every required key as a nonempty string
/// and no unknown keys; `props`, when present, is a nonempty object.
fn validate_expect(want: &Value) -> Result<(), String> {
    let object = want.as_object().ok_or("expect.json is not an object")?;
    if object.is_empty() {
        return Err("expect.json asserts nothing".into());
    }
    for (key, entries) in object {
        let (_, required, optional) = SECTIONS
            .iter()
            .find(|(name, _, _)| name == key)
            .ok_or_else(|| format!("unknown section {key}"))?;
        let entries = entries
            .as_array()
            .filter(|entries| !entries.is_empty())
            .ok_or_else(|| format!("{key} must be a nonempty array"))?;
        for entry in entries {
            let fields = entry
                .as_object()
                .ok_or_else(|| format!("{key} entry is not an object"))?;
            for field in *required {
                if fields
                    .get(*field)
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(format!("{key} entry lacks {field}"));
                }
            }
            for (field, value) in fields {
                if required.contains(&field.as_str()) {
                    continue;
                }
                if !optional.contains(&field.as_str()) {
                    return Err(format!("{key} entry has unknown key {field}"));
                }
                if value.as_object().is_none_or(serde_json::Map::is_empty) {
                    return Err(format!("{key} entry {field} must be a nonempty object"));
                }
            }
        }
    }
    Ok(())
}

fn missing(want: &Value, key: &str, got: &[Value]) -> Vec<String> {
    want[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| !got.iter().any(|g| subset(w, g)))
        .map(|w| format!("{key}: {w}"))
        .collect()
}

#[test]
fn every_claimed_cell_passes_its_conformance_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/ingest/conformance");
    let mut failures = Vec::new();
    for cell in COVERAGE_MATRIX {
        let Some(fixture) = cell.fixture() else {
            continue;
        };
        let dir = root.join(&fixture);
        let want: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("expect.json")).unwrap())
                .unwrap();
        if let Err(error) = validate_expect(&want) {
            failures.push(format!("{fixture}: {error}"));
            continue;
        }
        let (x, _) = crate::extract_tree_with_summary(
            &dir.join("src"),
            REPO,
            "workdir",
            &[],
            &BTreeMap::new(),
            None,
            None,
            &[],
        )
        .unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let nodes: Vec<Value> = x
            .nodes
            .iter()
            .map(|n| serde_json::json!({"label": n.label, "id": n.id, "props": n.props}))
            .collect();
        let edges: Vec<Value> = x
            .edges
            .iter()
            .map(|e| serde_json::json!({"label": e.label, "src": e.src, "dst": e.dst}))
            .collect();
        let events: Vec<Value> = x
            .event_sites
            .iter()
            .map(|s| {
                serde_json::json!({
                    "kind": s.kind,
                    "role": format!("{:?}", s.role),
                    "identity": identity(&s.identity),
                })
            })
            .collect();
        let fetches: Vec<Value> = x
            .fetch_sites
            .iter()
            .map(|s| serde_json::json!({"method": s.method, "url": identity(&s.url)}))
            .collect();
        let mut gaps = missing(&want, "nodes", &nodes);
        gaps.extend(missing(&want, "edges", &edges));
        gaps.extend(missing(&want, "event_sites", &events));
        gaps.extend(missing(&want, "fetch_sites", &fetches));
        failures.extend(gaps.into_iter().map(|g| format!("{fixture}: {g}")));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn vacuous_or_malformed_expectations_are_refused() {
    use serde_json::json;
    let edge = json!({"label": "CALLS", "src": "a", "dst": "b"});
    assert!(validate_expect(&json!({"edges": [edge]})).is_ok());
    assert!(
        validate_expect(
            &json!({"nodes": [{"label": "Endpoint", "id": "e", "props": {"method": "GET"}}]})
        )
        .is_ok()
    );
    for bad in [
        json!([]),
        json!({}),
        json!({"nodes": []}),
        json!({"nodez": [{"label": "Endpoint", "id": "e"}]}),
        json!({"nodes": "bad"}),
        json!({"nodes": [{}]}),
        json!({"nodes": ["Endpoint"]}),
        json!({"nodes": [{"label": "Endpoint"}]}),
        json!({"nodes": [{"label": "", "id": "e"}]}),
        json!({"nodes": [{"label": "Endpoint", "id": 7}]}),
        json!({"nodes": [{"label": "Endpoint", "id": "e", "props": {}}]}),
        json!({"nodes": [{"label": "Endpoint", "id": "e", "prop": {"a": 1}}]}),
        json!({"edges": [edge], "fetch_sites": []}),
    ] {
        assert!(validate_expect(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn a_client_only_rust_repository_still_yields_its_reqwest_calls() {
    // AC-0367 (#1093 review): reqwest calls are client facts, so a manifest
    // selecting only the client layer still runs the Rust pass.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/ingest/conformance/rust.reqwest.http_client_calls/src");
    let (x, _) = crate::extract_tree_with_summary(
        &dir,
        REPO,
        "workdir",
        &["client".to_string()],
        &BTreeMap::new(),
        None,
        None,
        &[],
    )
    .unwrap();
    let methods: Vec<&str> = x.fetch_sites.iter().map(|s| s.method.as_str()).collect();
    assert_eq!(methods, ["GET", "DELETE", "POST"]);
}
