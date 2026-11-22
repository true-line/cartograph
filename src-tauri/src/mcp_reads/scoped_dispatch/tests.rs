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
        let capabilities = &caps["result"]["structuredContent"];
        assert_eq!(capabilities["evidence_absence_reason_available"], false);
        // AC-0654: a scoped session reads no complete graph, computes no
        // curated projection and offers no curated read.
        assert_eq!(capabilities["h3_curated_available"], false);
        assert_eq!(
            capabilities["h3_curated_unavailable_reason"],
            "complete_basis_required"
        );
        assert_eq!(
            capabilities["receipt_omission_classification"]["available"],
            true
        );
        assert_eq!(
            capabilities["receipt_omission_classification"]["partial"],
            true
        );
        assert_eq!(
            capabilities["receipt_omission_classification"]["unknown_is_omitted"],
            false
        );
        assert_eq!(
            capabilities["receipt_omission_classification"]["kinds"],
            json!(["declarations", "rules", "store_capacity"])
        );
        assert_eq!(
            capabilities["forgotten_receipt_classification"]["available"],
            true
        );
        assert_eq!(
            capabilities["forgotten_receipt_classification"]["scope"],
            "exact_current_basis_binding_receipt_id"
        );
        assert_eq!(
            capabilities["forgotten_receipt_classification"]["partial"],
            true
        );
        assert_eq!(
            capabilities["forgotten_receipt_classification"]["omitted_available"],
            false
        );
        assert_eq!(
            capabilities["forgotten_receipt_classification"]["unknown_is_forgotten"],
            false
        );
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
        assert_eq!(
            entries.len(),
            crate::mcp_reads::artifacts::readable_catalog().len()
        );
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

/// orders -CALLS-> payments (Confirmed) -CALLS-> ledger (InferredWeak node
/// and edge), and audit -CALLS-> ledger: in a verified-only session the
/// hidden ledger must not bridge payments to audit.
fn relations_fixture(
    mode: spec::ExportMode,
    test: impl FnOnce(&mut ScopedDispatcher<'_>, &mut ScopedReadContext<'_>, &str),
) {
    use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
    let temp = tempfile::tempdir().unwrap();
    let state = crate::registered_source_tests::app_state(temp.path());
    let prov = |tier: ConfidenceTier, id: &str| {
        Provenance::new(
            Tier::Deterministic,
            tier,
            vec![EvidenceRef {
                repo: "fixture/repo".into(),
                path: "main.tf".into(),
                byte_start: 0,
                byte_end: 8,
                commit_sha: "fixed".into(),
            }],
            "fixture",
            id.as_bytes(),
        )
        .unwrap()
    };
    {
        let mut graph = state.graph.lock().unwrap();
        for (id, tier) in [
            ("svc:audit", ConfidenceTier::Confirmed),
            ("svc:ledger", ConfidenceTier::InferredWeak),
            ("svc:orders", ConfidenceTier::Confirmed),
            ("svc:payments", ConfidenceTier::Confirmed),
        ] {
            graph
                .put_node(&Node {
                    id: id.into(),
                    label: "Service".into(),
                    props: json!({"name":id,"prov":prov(tier, id)}),
                })
                .unwrap();
        }
        for (src, dst, tier) in [
            ("svc:orders", "svc:payments", ConfidenceTier::Confirmed),
            ("svc:payments", "svc:ledger", ConfidenceTier::InferredWeak),
            ("svc:audit", "svc:ledger", ConfidenceTier::Confirmed),
        ] {
            graph
                .put_edge(&core_graph::Edge {
                    src: src.into(),
                    dst: dst.into(),
                    label: "CALLS".into(),
                    props: json!({"prov":prov(tier, &format!("{src}>{dst}"))}),
                })
                .unwrap();
        }
    }
    let mut scope = || Ok(Vec::new());
    let mut ctx = ScopedReadContext {
        graph: &state.graph,
        decisions: &state.decisions,
        scope: &mut scope,
    };
    let limits = super::super::worker::tests::limits();
    let session =
        ScopedReadSession::acquire(&mut ctx, limits.graph, limits.coverage, mode).unwrap();
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
    dispatch.receive(
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        &mut ctx,
    );
    test(&mut dispatch, &mut ctx, &basis);
}
fn relation_refs(response: &Value) -> Vec<String> {
    response["result"]["structuredContent"]["relations"]["page"]["facts"]
        .as_array()
        .unwrap_or_else(|| panic!("{response}"))
        .iter()
        .map(|fact| match fact["reference"]["kind"].as_str().unwrap() {
            "node" => fact["reference"]["id"].as_str().unwrap().to_owned(),
            _ => format!(
                "{}-{}->{}",
                fact["reference"]["source"].as_str().unwrap(),
                fact["reference"]["label"].as_str().unwrap(),
                fact["reference"]["destination"].as_str().unwrap()
            ),
        })
        .collect()
}

// AC-1810 (#250): a scoped session answers "what does this node relate to"
// with the induced neighborhood of its visible projection at the pinned
// revision, paged under the same selection, without global claims.
#[test]
fn scoped_query_reads_a_visible_neighborhood_without_hidden_bridges() {
    let args = |basis: &str, anchor: &str, hops: usize| json!({"session_id":"fixture-session","session_basis_id":basis,"anchor":anchor,"hops":hops,"max_facts":50,"max_bytes":65536});
    relations_fixture(spec::ExportMode::VerifiedOnly, |dispatch, ctx, basis| {
        let caps = call(dispatch, ctx, 2, "cartograph_capabilities", json!({}));
        let relations = &caps["result"]["structuredContent"]["relations_access"];
        assert_eq!(relations["via"], "cartograph_query");
        assert_eq!(relations["query_tool_available"], true);
        assert_eq!(relations["scopes"], json!(["neighborhood"]));
        // Three hops from payments: the hidden ledger neither appears nor
        // connects payments to audit.
        let page = call(
            dispatch,
            ctx,
            3,
            "cartograph_query",
            args(basis, "svc:payments", 3),
        );
        assert_eq!(page["result"]["isError"], false, "{page}");
        let body = &page["result"]["structuredContent"]["relations"];
        assert_eq!(body["complete_system"], false);
        assert_eq!(body["absence_claim"], false);
        assert_eq!(
            relation_refs(&page),
            [
                "svc:orders",
                "svc:payments",
                "svc:orders-CALLS->svc:payments"
            ]
        );
        // A hidden anchor is refused without naming it.
        let error = call(
            dispatch,
            ctx,
            4,
            "cartograph_query",
            args(basis, "svc:ledger", 1),
        );
        assert_eq!(error["error"]["code"], -32602);
        assert!(!error.to_string().contains("ledger"));
    });
    relations_fixture(spec::ExportMode::BestEffort, |dispatch, ctx, basis| {
        let page = call(
            dispatch,
            ctx,
            2,
            "cartograph_query",
            args(basis, "svc:payments", 2),
        );
        assert_eq!(
            relation_refs(&page),
            [
                "svc:audit",
                "svc:ledger",
                "svc:orders",
                "svc:payments",
                "svc:audit-CALLS->svc:ledger",
                "svc:orders-CALLS->svc:payments",
                "svc:payments-CALLS->svc:ledger"
            ]
        );
        // Edges only, one per page, continued with the issued cursor.
        let mut request = args(basis, "svc:ledger", 1);
        request["kind"] = json!("edge");
        request["labels"] = json!(["CALLS"]);
        request["max_facts"] = json!(1);
        let first = call(dispatch, ctx, 3, "cartograph_query", request.clone());
        assert_eq!(relation_refs(&first), ["svc:audit-CALLS->svc:ledger"]);
        let page = &first["result"]["structuredContent"]["relations"]["page"];
        assert_eq!(page["total_selected"], 2);
        let mut next = request.clone();
        next["cursor"] = page["next_cursor"].clone();
        let second = call(dispatch, ctx, 4, "cartograph_query", next.clone());
        assert_eq!(relation_refs(&second), ["svc:payments-CALLS->svc:ledger"]);
        assert!(
            second["result"]["structuredContent"]["relations"]["page"]["next_cursor"].is_null()
        );
        // A cursor from another selection, a forged basis, an unknown
        // field and a radius past the bound are refused.
        let mut other = args(basis, "svc:payments", 1);
        other["cursor"] = page["next_cursor"].clone();
        let error = call(dispatch, ctx, 5, "cartograph_query", other);
        assert_eq!(
            error["result"]["structuredContent"]["error"], "stale_basis",
            "{error}"
        );
        let mut stale = next.clone();
        stale["session_basis_id"] = json!("forged");
        let error = call(dispatch, ctx, 6, "cartograph_query", stale);
        assert_eq!(error["result"]["structuredContent"]["error"], "stale_basis");
        let mut unknown = next;
        unknown["scope"] = json!("all");
        let error = call(dispatch, ctx, 7, "cartograph_query", unknown);
        assert_eq!(error["error"]["code"], -32602);
        let error = call(
            dispatch,
            ctx,
            8,
            "cartograph_query",
            args(basis, "svc:payments", 4),
        );
        assert_eq!(error["error"]["code"], -32602);
    });
}
