//! D1 §1.6 arranged per capability (#1985, AC-0859).
use super::*;
use crate::workbench::capabilities::Story;
use crate::workbench::render::Budget;
use core_graph::overlay::BaseRef;
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use serde_json::{Value, json};

fn prov(fact: &str) -> Value {
    let evidence = EvidenceRef {
        repo: "acme/mail".into(),
        path: "app/api/user.py".into(),
        byte_start: 10,
        byte_end: 40,
        commit_sha: "0123abc".into(),
    };
    serde_json::to_value(
        Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
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

fn edge(src: &str, dst: &str, label: &str, extra: Value) -> Edge {
    let mut props = extra;
    props["prov"] = prov(&format!("{src}{label}{dst}"));
    Edge {
        src: src.into(),
        dst: dst.into(),
        label: label.into(),
        props,
    }
}

const DELETE: &str = "ep:acme/mail@DELETE:/api/user";
const ADMIN: &str = "ep:acme/mail@GET:/admin/alias/";
const HEALTH: &str = "ep:acme/mail@GET:/health";
const DELETE_USER: &str = "sym:acme/mail@app/api/user.py#delete_user";
const AUDIT: &str = "sym:acme/mail@app/audit.py#emit_user_audit_log";
const ADMIN_VIEW: &str = "sym:acme/mail@app/admin.py#AliasAdmin.index_view";
const HEALTHY: &str = "sym:acme/mail@app/health.py#healthy";
const USER: &str = "data:acme/mail@User";
const LOG: &str = "data:acme/mail@UserAuditLog";

/// A capability with a flow that calls on and writes two entities, and an
/// operation handled with nothing further; an endpoint outside it.
fn fixture() -> (Vec<Node>, Vec<Edge>) {
    let nodes = vec![
        node(
            DELETE,
            "Endpoint",
            json!({"method": "DELETE", "path": "/api/user"}),
        ),
        node(
            ADMIN,
            "Endpoint",
            json!({"method": "GET", "path": "/admin/alias/"}),
        ),
        node(
            HEALTH,
            "Endpoint",
            json!({"method": "GET", "path": "/health"}),
        ),
        node(DELETE_USER, "Symbol", json!({"name": "delete_user"})),
        node(AUDIT, "Symbol", json!({"name": "emit_user_audit_log"})),
        node(
            ADMIN_VIEW,
            "Symbol",
            json!({"name": "AliasAdmin.index_view"}),
        ),
        node(HEALTHY, "Symbol", json!({"name": "healthy"})),
        node(USER, "DataEntity", json!({"name": "User"})),
        node(LOG, "DataEntity", json!({"name": "UserAuditLog"})),
    ];
    let edges = vec![
        edge(DELETE, DELETE_USER, "HANDLES", json!({})),
        edge(ADMIN, ADMIN_VIEW, "HANDLES", json!({})),
        edge(HEALTH, HEALTHY, "HANDLES", json!({})),
        edge(DELETE_USER, AUDIT, "CALLS", json!({})),
        edge(
            DELETE_USER,
            USER,
            "WRITES",
            json!({"operations": ["update"]}),
        ),
        edge(DELETE_USER, USER, "READS", json!({"operation": "select"})),
        edge(AUDIT, LOG, "WRITES", json!({"operations": ["insert"]})),
    ];
    (nodes, edges)
}

fn render() -> (String, usize, usize) {
    render_with(fixture())
}

fn render_with((nodes, edges): (Vec<Node>, Vec<Edge>)) -> (String, usize, usize) {
    let flows = flowtracer::trace(&nodes, &edges);
    let nodes: Vec<&Node> = nodes.iter().collect();
    let edges: Vec<&Edge> = edges.iter().collect();
    let refs: Vec<BaseRef> = edges[..2]
        .iter()
        .map(|e| BaseRef::Edge(e.src.clone(), e.dst.clone(), e.label.clone()))
        .collect();
    let capability = Node {
        id: "capability:test:account".into(),
        label: "Capability".into(),
        props: json!({ "title": "Manage account" }),
    };
    let knowledge = Stories {
        included: vec![Story {
            node: &capability,
            support: vec![
                (&refs[0], Support::Edge(edges[0])),
                (&refs[1], Support::Edge(edges[1])),
            ],
        }],
        filtered: 0,
        areas: BTreeMap::new(),
    };
    let budget = Budget::new(None).unwrap();
    let mut assertions = Assertions::new(&budget);
    let mut asserted = BTreeSet::new();
    let mut out = Text::new(&budget, "").unwrap();
    let cited = flows_per_capability(
        (&nodes, &edges),
        &flows,
        &knowledge,
        (&mut assertions, &mut asserted),
        &mut out,
    )
    .unwrap()
    .expect("a capability operation has a flow");
    (out.finish(), cited, assertions.values.len())
}

// AC-0859: an operation's line names its handler, the callables it reaches
// and the entities they write and read with their operations, citing the
// handler and data edges; a handler-only operation is listed together; a
// flow outside every capability keeps a table row.
#[test]
fn business_flows_describe_each_operation_per_capability() {
    let (text, cited, asserted) = render();
    assert!(
        text.contains("Traced flows per capability: 2 operations across 1 capabilities."),
        "{text}"
    );
    assert!(text.contains("### Manage account"), "{text}");
    let line = text
        .lines()
        .find(|l| l.starts_with("- **DELETE /api/user**"))
        .expect(&text);
    assert!(
        line.contains("(Verified): handled by delete\\_user"),
        "{line}"
    );
    assert!(line.contains("; calls emit\\_user\\_audit\\_log"), "{line}");
    assert!(
        line.contains("; writes User (update), UserAuditLog (insert)"),
        "{line}"
    );
    assert!(line.contains("; reads User (select)."), "{line}");
    // HANDLES, two WRITES and a READS are cited; the CALLS hop is not.
    assert_eq!(line.matches("F-").count(), 4, "{line}");
    assert_eq!((cited, asserted), (4, 4), "{text}");
    assert!(
        text.contains("- Handled with nothing further traced: GET /admin/alias/."),
        "{text}"
    );
    assert!(
        text.contains("### Flows outside the capabilities"),
        "{text}"
    );
    assert!(
        text.contains("| GET /health | Endpoint | Verified | 1 |"),
        "{text}"
    );
    // No raw ids.
    for id in ["ep:", "sym:", "data:"] {
        assert!(!text.contains(id), "{id} in {text}");
    }
}

// AC-0859: a callee that shares its handler's name is still listed, and a
// Gap-confidence edge is counted as unresolved, not named as reached.
#[test]
fn business_flows_keep_same_named_callees_and_count_gap_edges() {
    let (mut nodes, mut edges) = fixture();
    let twin = "sym:acme/mail@app/jobs.py#delete_user";
    let lost = "sym:acme/mail@app/lost.py#notify";
    nodes.push(node(twin, "Symbol", json!({"name": "delete_user"})));
    nodes.push(node(lost, "Symbol", json!({"name": "notify"})));
    edges.push(edge(DELETE_USER, twin, "CALLS", json!({})));
    let mut gap = edge(DELETE_USER, lost, "CALLS", json!({}));
    gap.props["prov"]["confidence_tier"] = json!("Gap");
    edges.push(gap);
    let (text, _, _) = render_with((nodes, edges));
    let line = text
        .lines()
        .find(|l| l.starts_with("- **DELETE /api/user**"))
        .expect(&text);
    assert!(line.contains("(Partial)"), "{line}");
    assert!(
        line.contains("; calls emit\\_user\\_audit\\_log, delete\\_user"),
        "{line}"
    );
    assert!(!line.contains("notify"), "{line}");
    assert!(line.contains("; 1 hop is unresolved"), "{line}");
}

// AC-0859: the workspace is charged at the graph ceilings and refuses
// overflow.
#[test]
fn flow_arrangement_workspace_is_checked() {
    let graph = |bytes, rows, values| flowtracer::TraceGraphBound {
        bytes,
        rows,
        values,
    };
    let small = flows_per_capability_workspace(graph(1 << 10, 16, 64)).unwrap();
    let large = flows_per_capability_workspace(graph(1 << 20, 1 << 10, 1 << 12)).unwrap();
    assert!(small > 0 && large > small);
    assert!(flows_per_capability_workspace(graph(usize::MAX, 1, 1)).is_none());
    assert!(flows_per_capability_workspace(graph(1, usize::MAX, 1)).is_none());
}

// AC-0893: a cited process start is an operation of its capability: its
// flow is placed under it, the named callables defined in the file it
// enters are its handlers, and what they call and write follows (#1997).
#[test]
fn entry_point_capabilities_list_their_operations_and_flows_in_d1() {
    const MAIL: &str = "entry:acme/mail@email_handler.py#main";
    const FILE: &str = "file:acme/mail@email_handler.py";
    const HANDLE: &str = "sym:acme/mail@email_handler.py#handle";
    const FORWARD: &str = "sym:acme/mail@app/forward.py#forward_email_to_mailbox";
    const PINS: &str = "decl:acme/mail@email_handler.py#PINS";
    const ANON: &str = "sym:acme/mail@email_handler.py#anon@120";
    let nodes = vec![
        node(
            MAIL,
            "EntryPoint",
            json!({"name": "Mail received over SMTP (email_handler.py, port 20381)"}),
        ),
        node(FILE, "File", json!({"path": "email_handler.py"})),
        node(HANDLE, "Symbol", json!({"name": "handle"})),
        node(
            FORWARD,
            "Symbol",
            json!({"name": "forward_email_to_mailbox"}),
        ),
        node(LOG, "DataEntity", json!({"name": "EmailLog"})),
        node(PINS, "Declaration", json!({"name": "PINS"})),
        node(ANON, "Symbol", json!({"name": "anon@120"})),
    ];
    let edges = vec![
        edge(MAIL, FILE, "ENTRY", json!({})),
        edge(HANDLE, FILE, "DEFINED_IN", json!({})),
        edge(PINS, FILE, "DEFINED_IN", json!({})),
        edge(ANON, FILE, "DEFINED_IN", json!({})),
        edge(HANDLE, FORWARD, "CALLS", json!({})),
        edge(FORWARD, LOG, "WRITES", json!({"operations": ["insert"]})),
    ];
    let flows = flowtracer::trace(&nodes, &edges);
    let node_refs: Vec<&Node> = nodes.iter().collect();
    let edge_refs: Vec<&Edge> = edges.iter().collect();
    let cite = BaseRef::Node(MAIL.into());
    let capability = Node {
        id: "capability:entry-point:mail".into(),
        label: "Capability".into(),
        props: json!({ "title": "Receive mail over SMTP" }),
    };
    let knowledge = Stories {
        included: vec![Story {
            node: &capability,
            support: vec![(&cite, Support::Node(&nodes[0]))],
        }],
        filtered: 0,
        areas: BTreeMap::new(),
    };
    let budget = Budget::new(None).unwrap();
    let mut assertions = Assertions::new(&budget);
    let mut asserted = BTreeSet::new();
    let mut out = Text::new(&budget, "").unwrap();
    flows_per_capability(
        (&node_refs, &edge_refs),
        &flows,
        &knowledge,
        (&mut assertions, &mut asserted),
        &mut out,
    )
    .unwrap()
    .expect("the entry point's flow is placed");
    let text = out.finish();
    assert!(text.contains("### Receive mail over SMTP"), "{text}");
    let line = text
        .lines()
        .find(|l| l.starts_with("- **Mail received over SMTP"))
        .expect(&text);
    // Only the file's named callables handle it (AC-0893): not its
    // constants, and not its anonymous functions.
    assert!(line.contains("handled by handle;"), "{line}");
    assert!(
        line.contains("; calls forward\\_email\\_to\\_mailbox"),
        "{line}"
    );
    assert!(line.contains("; writes EmailLog (insert)"), "{line}");
    assert!(!text.contains("Flows outside the capabilities"), "{text}");
}
