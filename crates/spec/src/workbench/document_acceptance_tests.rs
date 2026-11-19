//! D1 §1.4 arranged per capability (#1443, AC-0850).
use super::*;
use crate::workbench::render::Budget;
use core_graph::overlay::BaseRef;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

fn prov(fact: &str) -> Value {
    let evidence = EvidenceRef {
        repo: "acme/shop".into(),
        path: "e2e/order.e2e-spec.ts".into(),
        byte_start: 10,
        byte_end: 40,
        commit_sha: "0123abc".into(),
    };
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::InferredStrong,
            vec![evidence],
            "t0.fixture",
            fact.as_bytes(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn node(id: &str, label: &str, props: Value) -> Node {
    let mut props = props;
    props["prov"] = prov(id);
    Node {
        id: id.into(),
        label: label.into(),
        props,
    }
}

fn edge(src: &str, dst: &str, label: &str) -> Edge {
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props: json!({ "prov": prov(&format!("{src}{label}{dst}")) }),
    }
}

const ADD: &str = "ep:acme/shop@POST:/orders/items";
const LIST: &str = "ep:acme/shop@GET:/orders";
const OTHER: &str = "ep:acme/shop@GET:/health";
const HANDLER: &str = "sym:acme/shop@src/order.ts#OrderController.add";

/// Two operations of one capability, one operation outside it, a test that
/// requests both of the capability's, one that requests one, and one that
/// requests only the outside operation.
fn fixture() -> (Vec<Node>, Vec<Edge>) {
    let nodes = vec![
        node(
            ADD,
            "Endpoint",
            json!({"method": "POST", "path": "/orders/items"}),
        ),
        node(
            LIST,
            "Endpoint",
            json!({"method": "GET", "path": "/orders"}),
        ),
        node(
            OTHER,
            "Endpoint",
            json!({"method": "GET", "path": "/health"}),
        ),
        node(HANDLER, "Symbol", json!({"name": "add"})),
        node(
            "test:a",
            "TestCase",
            json!({"name": "rejects a negative quantity", "path": "e2e/order.e2e-spec.ts",
                   "qualified_name": "Orders > addItem > rejects a negative quantity"}),
        ),
        node(
            "test:b",
            "TestCase",
            json!({"name": "lists orders", "path": "e2e/order.e2e-spec.ts",
                   "qualified_name": "Orders > lists orders"}),
        ),
        node(
            "test:c",
            "TestCase",
            json!({"name": "test_is_healthy", "path": "tests/test_health.py",
                   "qualified_name": "test_is_healthy"}),
        ),
    ];
    let edges = vec![
        edge("test:a", ADD, "REQUESTS"),
        edge("test:a", LIST, "REQUESTS"),
        edge("test:b", LIST, "REQUESTS"),
        edge("test:c", OTHER, "REQUESTS"),
    ];
    (nodes, edges)
}

fn render(cited_handles: bool) -> (String, usize, usize) {
    let (nodes, edges) = fixture();
    let nodes: Vec<&Node> = nodes.iter().collect();
    let edges: Vec<&Edge> = edges.iter().collect();
    // The capability cites the HANDLES edges of its two operations; a
    // capability that cites only its handler Symbol has no operations.
    let handles = [
        edge(ADD, HANDLER, "HANDLES"),
        edge(LIST, HANDLER, "HANDLES"),
    ];
    let refs: Vec<BaseRef> = handles
        .iter()
        .map(|e| BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone()))
        .chain([BaseRef::Node(HANDLER.into())])
        .collect();
    let handler = nodes.iter().copied().find(|n| n.id == HANDLER).unwrap();
    let capability = Node {
        id: "capability:test:orders".into(),
        label: "Capability".into(),
        props: json!({ "title": "Orders" }),
    };
    let support = if cited_handles {
        vec![
            (&refs[0], Support::Edge(&handles[0])),
            (&refs[1], Support::Edge(&handles[1])),
        ]
    } else {
        vec![(&refs[2], Support::Node(handler))]
    };
    let knowledge = Stories {
        included: vec![Story {
            node: &capability,
            support,
        }],
        filtered: 0,
    };
    let budget = Budget::new(None).unwrap();
    let mut assertions = Assertions::new(&budget);
    let mut asserted = BTreeSet::new();
    let mut out = Text::new(&budget, "").unwrap();
    let printed = acceptance_per_capability(
        (&nodes, &edges),
        &knowledge,
        (&mut assertions, &mut asserted),
        &mut out,
    )
    .unwrap();
    (out.finish(), printed, assertions.values.len())
}

// AC-0850: each test that requests a capability's operation is listed once
// under it, by its qualified name, with every operation it requests and the
// citation of each REQUESTS edge; a test of another operation is not.
#[test]
fn tests_requesting_a_capability_operation_are_its_acceptance_scenarios() {
    let (text, printed, cited) = render(true);
    assert_eq!((printed, cited), (2, 3), "{text}");
    assert!(
        text.contains("Acceptance scenarios per capability: 2."),
        "{text}"
    );
    assert!(text.contains("### Orders\n"), "{text}");
    let both = text
        .lines()
        .find(|l| l.starts_with("| Orders &gt; addItem &gt; rejects a negative quantity |"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        both.contains("| e2e/order.e2e-spec.ts | GET /orders; POST /orders/items | F-"),
        "{both}"
    );
    assert_eq!(both.matches("F-").count(), 2, "{both}");
    assert!(text.contains("| Orders &gt; lists orders | e2e/order.e2e-spec.ts | GET /orders |"));
    assert!(!text.contains("healthy"), "{text}");
    assert!(!text.contains("/health"), "{text}");
    // Without a cited HANDLES edge a capability has no operations.
    let (text, printed, _) = render(false);
    assert_eq!((printed, text.as_str()), (0, ""));
}

// AC-0850: the workspace is charged at the graph ceilings and refuses
// overflow.
#[test]
fn acceptance_arrangement_workspace_is_checked() {
    let graph = |bytes, rows, values| flowtracer::TraceGraphBound {
        bytes,
        rows,
        values,
    };
    let small = acceptance_per_capability_workspace(graph(1 << 10, 16, 64)).unwrap();
    let large = acceptance_per_capability_workspace(graph(1 << 20, 1 << 10, 1 << 12)).unwrap();
    assert!(small > 0 && large > small);
    assert!(acceptance_per_capability_workspace(graph(usize::MAX, 1, 1)).is_none());
    assert!(acceptance_per_capability_workspace(graph(1, usize::MAX, 1)).is_none());
}

// AC-0850: a snake_case test name is shown as words; a written name is kept.
#[test]
fn test_names_read_as_words() {
    assert_eq!(
        readable("test_link_callback_does_not_grant_sudo"),
        "link callback does not grant sudo"
    );
    assert_eq!(
        readable("TestOrders > test_rejects__negative"),
        "TestOrders > rejects negative"
    );
    assert_eq!(
        readable("Orders > lists orders_by date"),
        "Orders > lists orders_by date"
    );
}
