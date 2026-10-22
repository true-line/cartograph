use super::*;
use core_graph::{Edge, GraphPatch};
fn confirmed(id: &str, label: &str) -> Node {
    let prov = core_prov::Provenance::new(
        core_prov::Tier::Deterministic,
        core_prov::ConfidenceTier::Confirmed,
        vec![core_prov::EvidenceRef {
            repo: "fixture/repo".into(),
            path: "model.py".into(),
            byte_start: 0,
            byte_end: 1,
            commit_sha: "abc".into(),
        }],
        "partition.fixture",
        id.as_bytes(),
    )
    .unwrap();
    Node {
        id: id.into(),
        label: label.into(),
        props: json!({"name":id,"prov":prov}),
    }
}
#[test]
fn scoped_partition_large_estate_identity_envelope_and_staleness() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(temp.path());
    let mut nodes: Vec<_> = (0..60_001)
        .map(|i| Node {
            id: format!("noise:{i}"),
            label: "Symbol".into(),
            props: json!({}),
        })
        .collect();
    nodes.extend([
        confirmed("entity:a", "DataEntity"),
        confirmed("entity:b", "DataEntity"),
    ]);
    let patch = GraphPatch {
        upsert_nodes: nodes,
        upsert_edges: vec![Edge {
            src: "entity:a".into(),
            dst: "entity:b".into(),
            label: "REFERENCES".into(),
            props: confirmed("relation", "Symbol").props,
        }],
        ..GraphPatch::default()
    };
    assert!(
        state
            .graph
            .lock()
            .unwrap()
            .apply_patch_if_snapshot_matches(&(vec![], vec![]), &patch)
            .unwrap()
    );
    drop(patch);
    let mut scope = || Ok(Vec::new());
    let mut ctx = ScopedReadContext {
        graph: &state.graph,
        decisions: &state.decisions,
        scope: &mut scope,
    };
    let limits = crate::mcp_reads::worker::tests::limits();
    let session = ScopedReadSession::acquire(
        &mut ctx,
        limits.graph,
        limits.coverage,
        spec::ExportMode::VerifiedOnly,
    )
    .unwrap();
    let basis = session.basis_id().to_owned();
    let mut dispatch = ScopedDispatcher::new(
        session,
        "fixture-session".into(),
        limits.dispatch,
        &state,
        limits.evidence,
    )
    .unwrap();
    dispatch.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),&mut ctx);
    dispatch.receive(
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        &mut ctx,
    );
    let args = |root: &str, max| json!({"session_id":"fixture-session","session_basis_id":basis,"kind":"data-model","root":root,"max_bytes":max});
    let first = call(
        &mut dispatch,
        &mut ctx,
        2,
        "cartograph_read_artifact",
        args("entity:a", 65536),
    );
    let second = call(
        &mut dispatch,
        &mut ctx,
        3,
        "cartograph_read_artifact",
        args("entity:b", 65536),
    );
    let a = &first["result"]["structuredContent"]["partition"];
    let b = &second["result"]["structuredContent"]["partition"];
    assert_eq!(a["complete_system"], false);
    assert_eq!(a["requested_root"], "entity:a");
    assert_eq!(b["requested_root"], "entity:b");
    assert!(
        a["artifact"]["content_id"]
            .as_str()
            .unwrap()
            .contains("partition")
    );
    assert_eq!(a["artifact"]["content_id"], b["artifact"]["content_id"]);
    assert_eq!(a["artifact"]["content"], b["artifact"]["content"]);
    assert_eq!(a["artifact"]["roots"], json!(["entity:a", "entity:b"]));
    assert!(a.get("recovered_graph_id").is_none());
    let exact = serde_json::to_vec(&first).unwrap().len();
    let accepted = call(
        &mut dispatch,
        &mut ctx,
        4,
        "cartograph_read_artifact",
        args("entity:a", exact),
    );
    assert_eq!(
        accepted["result"]["structuredContent"]["partition"]["artifact"]["content_id"],
        a["artifact"]["content_id"]
    );
    let refused = call(
        &mut dispatch,
        &mut ctx,
        5,
        "cartograph_read_artifact",
        args("entity:a", exact - 1),
    );
    assert_eq!(
        refused["result"]["structuredContent"]["error"],
        "response_limit"
    );
    let mut unsupported = args("entity:a", 65536);
    unsupported["kind"] = json!("flow-dossiers");
    assert_eq!(
        call(
            &mut dispatch,
            &mut ctx,
            6,
            "cartograph_read_artifact",
            unsupported
        )["error"]["code"],
        -32602
    );
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&confirmed("unrelated-change", "Symbol"))
        .unwrap();
    let stale = call(
        &mut dispatch,
        &mut ctx,
        7,
        "cartograph_read_artifact",
        args("entity:a", 65536),
    );
    assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
}
