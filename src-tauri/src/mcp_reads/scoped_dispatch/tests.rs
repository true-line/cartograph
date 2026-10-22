use super::*;
use core_graph::{GraphStore, Node};
fn fixture(test: impl FnOnce(&mut ScopedDispatcher<'_>, &mut ScopedReadContext<'_>, &str)) {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(temp.path());
    state
        .graph
        .lock()
        .unwrap()
        .put_node(&Node {
            id: "selected".into(),
            label: "Symbol".into(),
            props: json!({"name":"selected","implementation":"only in selected detail"}),
        })
        .unwrap();
    let mut scope = || Ok(Vec::new());
    let mut ctx = ScopedReadContext {
        graph: &state.graph,
        decisions: &state.decisions,
        scope: &mut scope,
    };
    let limits = super::super::worker::tests::limits();
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
    assert!(matches!(dispatch.receive(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),&mut ctx),DispatchAction::Respond(_)));
    assert_eq!(
        dispatch.receive(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            &mut ctx
        ),
        DispatchAction::Ignore
    );
    test(&mut dispatch, &mut ctx, &basis);
}
fn call(
    dispatch: &mut ScopedDispatcher<'_>,
    ctx: &mut ScopedReadContext<'_>,
    id: usize,
    name: &str,
    args: Value,
) -> Value {
    match dispatch.receive(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}),ctx) {DispatchAction::Respond(value)=>value,other=>panic!("unexpected action {other:?}")}
}
#[test]
fn scoped_protocol_advertises_real_routes_and_keeps_discovery_lean() {
    fixture(|dispatch, ctx, basis| {
        let tools =
            match dispatch.receive(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), ctx) {
                DispatchAction::Respond(v) => v,
                _ => panic!(),
            };
        let names: Vec<_> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"cartograph_read_fact"));
        assert!(names.contains(&"cartograph_read_artifact"));
        let caps = call(dispatch, ctx, 3, "cartograph_capabilities", json!({}));
        assert_eq!(
            caps["result"]["structuredContent"]["session_basis_id"],
            basis
        );
        assert_eq!(caps["result"]["structuredContent"]["mode"], "scoped");
        let discovery = call(
            dispatch,
            ctx,
            4,
            "cartograph_discover",
            json!({"session_id":"fixture-session","session_basis_id":basis,"request":{"query":{"by":"identifier","id":"selected"},"labels":[],"max_scan_rows":1,"max_candidates":1,"max_bytes":65536,"cursor":null}}),
        );
        assert!(!discovery.to_string().contains("only in selected detail"));
        assert_eq!(
            discovery["result"]["structuredContent"]["discovery"]["candidates"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let detail = call(
            dispatch,
            ctx,
            5,
            "cartograph_read_fact",
            json!({"session_id":"fixture-session","session_basis_id":basis,"fact":{"kind":"node","id":"selected"},"max_bytes":65536}),
        );
        assert_eq!(
            detail["result"]["structuredContent"]["detail"]["fact"]["properties"]["implementation"],
            "only in selected detail"
        );
        let catalog = call(
            dispatch,
            ctx,
            6,
            "cartograph_artifacts",
            json!({"session_id":"fixture-session","session_basis_id":basis}),
        );
        let entries = catalog["result"]["structuredContent"]["catalog"]["artifacts"]
            .as_array()
            .unwrap();
        assert_eq!(entries.len(), spec::artifact_catalog().len());
        assert_eq!(
            entries
                .iter()
                .filter(|e| e["status"] == "partition_supported")
                .count(),
            3
        );
        assert!(entries.iter().all(|e| (e["status"] == "unavailable"
            || e["status"] == "partition_supported")
            && e.get("content_id").is_none()));
    });
}
#[test]
fn strict_arguments_stale_basis_and_whole_envelope_limits_fail_closed() {
    fixture(|dispatch, ctx, basis| {
        let bad = call(
            dispatch,
            ctx,
            2,
            "cartograph_read_fact",
            json!({"session_id":"wrong","session_basis_id":basis,"fact":{"kind":"node","id":"selected"},"max_bytes":65536}),
        );
        assert_eq!(bad["result"]["structuredContent"]["error"], "stale_basis");
        let bad = call(
            dispatch,
            ctx,
            3,
            "cartograph_read_fact",
            json!({"session_id":"fixture-session","session_basis_id":basis,"fact":{"kind":"node","id":"selected","path":"forged"},"max_bytes":65536}),
        );
        assert_eq!(bad["error"]["code"], -32602);
        let bad = call(
            dispatch,
            ctx,
            4,
            "cartograph_discover",
            json!({"session_id":"fixture-session","session_basis_id":basis,"request":{"query":{"by":"identifier","id":"selected","extra":true},"labels":[],"max_scan_rows":1,"max_candidates":1,"max_bytes":65536,"cursor":null}}),
        );
        assert_eq!(bad["error"]["code"], -32602);
        // A tiny entire-response budget is refused before scoped detail/data
        // construction; even the error cannot fit, so this connection closes.
        assert_eq!(dispatch.receive(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cartograph_read_fact","arguments":{"session_id":"fixture-session","session_basis_id":basis,"fact":{"kind":"node","id":"selected"},"max_bytes":1}}}),ctx),DispatchAction::Close);
        assert_eq!(
            dispatch.receive(json!({"jsonrpc":"2.0","id":6,"method":"ping"}), ctx),
            DispatchAction::Close
        );
    });
}
#[test]
fn scoped_protocol_rejects_unadvertised_content_stale_revisions_and_reused_ids() {
    fixture(|dispatch, ctx, basis| {
        let unknown = call(dispatch, ctx, 2, "cartograph_read_artifact", json!({}));
        assert_eq!(unknown["error"]["code"], -32602);
        ctx.graph
            .lock()
            .unwrap()
            .put_node(&Node {
                id: "changed".into(),
                label: "Symbol".into(),
                props: json!({}),
            })
            .unwrap();
        let stale = call(
            dispatch,
            ctx,
            3,
            "cartograph_read_fact",
            json!({"session_id":"fixture-session","session_basis_id":basis,"fact":{"kind":"node","id":"selected"},"max_bytes":65536}),
        );
        assert_eq!(stale["result"]["structuredContent"]["error"], "stale_basis");
        assert_eq!(
            dispatch.receive(json!({"jsonrpc":"2.0","id":3,"method":"ping"}), ctx),
            DispatchAction::Close
        );
    });
}
#[test]
fn scoped_protocol_model_reserves_real_descriptors_when_write_cap_is_tiny() {
    let mut limits = super::super::worker::tests::limits();
    limits.dispatch.max_response_bytes = std::num::NonZeroUsize::new(1).unwrap();
    let bound = logical_protocol_workspace_bytes(limits).unwrap();
    let full = json!({"tools":TOOLS.iter().map(|t|schemas::descriptor(*t,limits.graph)).collect::<Vec<_>>()});
    assert!(
        bound
            >= context_hub::logical::json_bytes(context_hub::logical::count(&full).unwrap())
                .unwrap()
    );
}

#[path = "evidence_tests.rs"]
mod evidence;

mod partition_tests;
