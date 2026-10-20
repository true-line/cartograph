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
        if gaps.is_empty() && want.as_object().is_none_or(|o| o.is_empty()) {
            gaps.push("expect.json asserts nothing".into());
        }
        failures.extend(gaps.into_iter().map(|g| format!("{fixture}: {g}")));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
