//! #718 / AC-0288..0289 app dispatcher fixtures, not live-client acceptance.
use super::*;
use agents::DecisionLog;
use core_graph::coverage::CoverageReadLimits;
use core_graph::{GraphStore, Node, SnapshotReadLimits, SqliteGraphStore};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::sync::Mutex;

fn cap(bytes: usize) -> NonZeroUsize {
    NonZeroUsize::new(bytes).unwrap()
}
fn limits() -> DispatchLimits {
    DispatchLimits {
        max_response_bytes: cap(65536),
        max_request_ids: cap(128),
        trace: flowtracer::TraceLimits {
            max_flows: 64,
            max_hops: 512,
            max_output_bytes: 1024 * 1024,
        },
        compile: spec::SelectedCompileLimits {
            max_bytes: 1024 * 1024,
            adr: spec::AdrLimits {
                max_rows: 1024,
                max_output_bytes: 1024 * 1024,
            },
        },
    }
}
fn session() -> ReadSession {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
    for id in ["resource:orders", "resource:payments"] {
        let provenance = Provenance::new(
            Tier::Deterministic,
            ConfidenceTier::Confirmed,
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
        .unwrap();
        graph.put_node(&Node {id:id.into(),label:"Resource".into(),props:json!({"logical_id":format!("{id} é🙂\"\\"),"type":"queue","prov":provenance})}).unwrap();
    }
    ReadSession::acquire(
        &Mutex::new(graph),
        &Mutex::new(DecisionLog::open(":memory:").unwrap()),
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        spec::ExportMode::BestEffort,
    )
    .unwrap()
}
fn new(limits: DispatchLimits) -> Dispatcher<'static> {
    Dispatcher::new(session(), "opaque-fixture-session".into(), limits).unwrap()
}
fn rpc(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn init(dispatch: &mut Dispatcher) {
    let response=respond(dispatch.receive(rpc(json!("initialize"),"initialize",json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}))));
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        dispatch.receive(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
        DispatchAction::Ignore
    );
}
fn respond(action: DispatchAction) -> Value {
    match action {
        DispatchAction::Respond(response) => response,
        other => panic!("expected response: {other:?}"),
    }
}
fn call(dispatch: &mut Dispatcher, id: usize, tool: &str, args: Value) -> Value {
    respond(dispatch.receive(rpc(
        json!(id),
        "tools/call",
        json!({"name":tool,"arguments":args}),
    )))
}
fn identity(dispatch: &Dispatcher) -> Value {
    json!({"session_id":dispatch.session_id,"basis_id":dispatch.basis_id})
}
fn artifact_args(dispatch: &Dispatcher) -> Value {
    let mut args = identity(dispatch);
    args["artifact_id"] = json!("topology");
    args["offset"] = json!(0);
    args["max_content_bytes"] = json!(65536);
    args["max_bytes"] = json!(65536);
    args
}
fn discovery_args(dispatch: &Dispatcher) -> Value {
    let mut args = identity(dispatch);
    args["request"] = json!({"request":{"query":{"by":"text","text":"resource"},"labels":[],"max_candidates":1,"max_bytes":65536}});
    args
}

#[test]
fn lifecycle_and_real_schemas_advertise_only_five_implemented_reads() {
    let mut dispatch = new(limits());
    let before = respond(dispatch.receive(rpc(json!(0), "tools/list", json!({}))));
    assert!(before.get("error").is_some());
    init(&mut dispatch);
    let response = respond(dispatch.receive(rpc(json!(1), "tools/list", json!({}))));
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5);
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        IMPLEMENTED.map(ReadTool::name)
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["inputSchema"]["additionalProperties"] == false
                && tool["annotations"]["readOnlyHint"] == true)
    );
    let discover = tools
        .iter()
        .find(|tool| tool["name"] == "cartograph_discover")
        .unwrap();
    assert_eq!(
        discover["inputSchema"]["properties"]["request"]["properties"]["request"]["additionalProperties"],
        false
    );
    assert_eq!(
        discover["inputSchema"]["properties"]["request"]["properties"]["request"]["properties"]["query"]
            ["oneOf"][0]["additionalProperties"],
        false
    );
    assert!(!response.to_string().contains("cartograph_evidence"));
}

#[test]
fn real_capabilities_discovery_catalog_and_identity_bound_artifact_pages() {
    let mut dispatch = new(limits());
    init(&mut dispatch);
    let capabilities = call(&mut dispatch, 1, "cartograph_capabilities", json!({}));
    let data = &capabilities["result"]["structuredContent"];
    assert_eq!(data["basis_id"], dispatch.basis_id);
    assert_eq!(data["session_id"], dispatch.session_id);
    // AC-0654: this session computed the projection.
    assert_eq!(data["h3_curated_available"], true);
    assert_eq!(data["basis"]["h3_curated_available"], true);
    assert_eq!(data["basis"]["curated"]["status"], "computed");
    assert_eq!(
        data["curated_access"],
        json!({"via":"official_artifact","artifact_id":"curated","structured_artifact_id":"curated-json","revision":"basis.curated"})
    );
    assert_eq!(data["evidence_available"], false);
    assert_eq!(data["evidence_absence_reason_available"], false);
    assert_eq!(data["structured_content_required"], true);
    assert_eq!(data["limits"]["graph"]["scope"], "complete_recovered_graph");
    assert_eq!(data["limits"]["graph"]["hard_max_rows"], 10_000_000);
    assert_eq!(
        data["limits"]["graph"]["max_rows"],
        dispatch.session.graph_limits.max_rows
    );
    assert_eq!(
        data["limits"]["graph"]["max_bytes"],
        dispatch.session.graph_limits.max_bytes
    );
    assert_eq!(data["context_fact_kinds"], json!(["node", "edge"]));
    assert_eq!(data["discovery_fact_kinds"], json!(["node"]));
    assert_eq!(data["flows_access"]["artifact_id"], "flow-dossiers");
    assert_eq!(data["receipt_omission_classification"]["available"], false);
    assert_eq!(data["evidence_absence_reason_available"], false);
    assert_eq!(
        data["forgotten_receipt_classification"]["available"],
        data["evidence_available"]
    );
    assert_eq!(data["forgotten_receipt_classification"]["partial"], true);
    assert_eq!(
        data["forgotten_receipt_classification"]["omitted_available"],
        false
    );
    assert_eq!(
        data["forgotten_receipt_classification"]["unknown_is_forgotten"],
        false
    );
    assert_eq!(data["receipt_omission_classification"]["partial"], true);
    assert_eq!(
        data["receipt_omission_classification"]["kinds"],
        json!(["declarations", "rules", "store_capacity"])
    );
    assert_eq!(
        data["receipt_omission_classification"]["unknown_is_omitted"],
        false
    );
    assert_eq!(
        data["receipt_omission_classification"]["not_omitted_is_receipt_available"],
        false
    );
    let args = discovery_args(&dispatch);
    let first = call(&mut dispatch, 2, "cartograph_discover", args.clone());
    let context = &first["result"]["structuredContent"]["context"];
    assert_eq!(context["coverage"]["exhaustive"], false);
    assert_eq!(context["response"]["total_candidates"], 2);
    let mut continued = args;
    continued["request"]["context_id"] = context["context_id"].clone();
    continued["request"]["request"]["cursor"] = context["response"]["next_cursor"].clone();
    let second = call(&mut dispatch, 3, "cartograph_discover", continued);
    assert_ne!(
        first["result"]["structuredContent"]["context"]["response"]["candidates"],
        second["result"]["structuredContent"]["context"]["response"]["candidates"]
    );
    let args = identity(&dispatch);
    let catalog = call(&mut dispatch, 4, "cartograph_artifacts", args);
    assert_eq!(
        catalog["result"]["structuredContent"]["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        crate::mcp_reads::artifacts::readable_catalog().len()
    );
    assert!(!catalog.to_string().contains("content_id"));
    let mut args = artifact_args(&dispatch);
    args["max_content_bytes"] = json!(16);
    let first = call(&mut dispatch, 5, "cartograph_read_artifact", args.clone());
    let page = &first["result"]["structuredContent"]["artifact"];
    assert_eq!(page["partial"], true);
    assert_eq!(page["structured_assertions_included"], false);
    args["offset"] = page["next_offset"].clone();
    args["expected_content_id"] = page["content_id"].clone();
    let second = call(&mut dispatch, 6, "cartograph_read_artifact", args);
    assert_eq!(
        page["content_id"],
        second["result"]["structuredContent"]["artifact"]["content_id"]
    );
    assert_eq!(second["result"]["isError"], false);
}

fn unreviewable_session() -> ReadSession {
    ReadSession::acquire_with_overlay(
        &Mutex::new(SqliteGraphStore::open_in_memory().unwrap()),
        None,
        &Mutex::new(DecisionLog::open(":memory:").unwrap()),
        &crate::curated::ReviewsUnreadable,
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        spec::ExportMode::BestEffort,
    )
    .unwrap()
}

#[test]
fn curated_read_and_capability_flag_follow_the_session_projection() {
    // AC-0654 (ADR-0061 decisions 3 and 11): the curated read is the
    // `curated` artifact and its structured form, read with the artifact
    // tool. The flag is true where the session computed the projection and
    // false, with the reason in the basis, where it did not; the read then
    // returns the not-computed statement and no other read fails.
    let mut dispatch = new(limits());
    init(&mut dispatch);
    let args = identity(&dispatch);
    let catalog = call(&mut dispatch, 1, "cartograph_artifacts", args);
    let ids: Vec<_> = catalog["result"]["structuredContent"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap().to_owned())
        .collect();
    let at = ids.iter().position(|id| id == "curated").expect("curated");
    assert_eq!(ids[at + 1], "curated-json");
    let capabilities = call(&mut dispatch, 2, "cartograph_capabilities", json!({}));
    let revision = capabilities["result"]["structuredContent"]["basis"]["curated"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut args = artifact_args(&dispatch);
    args["artifact_id"] = json!("curated");
    let page = call(&mut dispatch, 3, "cartograph_read_artifact", args.clone());
    assert_eq!(page["result"]["isError"], false);
    let page = &page["result"]["structuredContent"]["artifact"];
    assert_eq!(page["artifact_id"], "curated");
    assert_eq!(page["partial"], false);
    assert!(page["content"].as_str().unwrap().contains(&revision));
    args["artifact_id"] = json!("curated-json");
    let page = call(&mut dispatch, 4, "cartograph_read_artifact", args);
    let page = &page["result"]["structuredContent"]["artifact"];
    assert_eq!(page["schema"], "cartograph.mcp/curated-json@1");
    let data: Value = serde_json::from_str(page["content"].as_str().unwrap()).unwrap();
    assert_eq!(data["view"], "curated");
    assert_eq!(data["revision"], revision.as_str());
    assert_eq!(data["counts"]["accepted"], 0);

    let mut dispatch = Dispatcher::new(
        unreviewable_session(),
        "opaque-fixture-session".into(),
        limits(),
    )
    .unwrap();
    init(&mut dispatch);
    let capabilities = call(&mut dispatch, 1, "cartograph_capabilities", json!({}));
    let data = &capabilities["result"]["structuredContent"];
    assert_eq!(data["h3_curated_available"], false);
    assert_eq!(data["basis"]["h3_curated_available"], false);
    assert_eq!(
        data["basis"]["curated"],
        json!({"status":"not_computed","reason":"reviews_unavailable"})
    );
    let mut args = artifact_args(&dispatch);
    let topology = call(&mut dispatch, 2, "cartograph_read_artifact", args.clone());
    assert_eq!(topology["result"]["isError"], false);
    args["artifact_id"] = json!("curated");
    let page = call(&mut dispatch, 3, "cartograph_read_artifact", args.clone());
    assert_eq!(page["result"]["isError"], false);
    assert!(
        page["result"]["structuredContent"]["artifact"]["content"]
            .as_str()
            .unwrap()
            .contains("reviews_unavailable")
    );
    args["artifact_id"] = json!("curated-json");
    let page = call(&mut dispatch, 4, "cartograph_read_artifact", args);
    let data: Value = serde_json::from_str(
        page["result"]["structuredContent"]["artifact"]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(data["status"], "not_computed");
    assert_eq!(data["failure"]["reason"], "reviews_unavailable");
}

#[test]
fn forged_session_content_and_unknown_nested_arguments_fail_without_diagnostics() {
    let mut dispatch = new(limits());
    init(&mut dispatch);
    let mut stale = identity(&dispatch);
    stale["basis_id"] = json!("forged");
    let error = call(&mut dispatch, 1, "cartograph_artifacts", stale);
    assert_eq!(error["result"]["isError"], true);
    assert_eq!(error["result"]["structuredContent"]["error"], "stale_basis");
    let mut args = artifact_args(&dispatch);
    args["expected_content_id"] = json!("forged");
    let error = call(&mut dispatch, 2, "cartograph_read_artifact", args);
    assert_eq!(error["result"]["structuredContent"]["error"], "stale_basis");
    let mut args = artifact_args(&dispatch);
    args["offset"] = json!(1);
    let error = call(&mut dispatch, 3, "cartograph_read_artifact", args);
    assert_eq!(error["error"]["code"], -32602);
    let mut cases = Vec::new();
    let base = discovery_args(&dispatch);
    for target in ["outer", "qualified", "request", "query", "cursor"] {
        let mut args = base.clone();
        match target {
            "outer" => args["path"] = json!("/private/secret"),
            "qualified" => args["request"]["sql"] = json!("DELETE FROM graph"),
            "request" => args["request"]["request"]["task"] = json!({}),
            "query" => args["request"]["request"]["query"]["write"] = json!(true),
            _ => {
                args["request"]["request"]["cursor"] = json!({"snapshot_id":"x","query_id":"x","policy":"x","offset":0,"execute":"secret"})
            }
        };
        cases.push(args);
    }
    for (index, args) in cases.into_iter().enumerate() {
        let error = call(&mut dispatch, index + 4, "cartograph_discover", args);
        assert_eq!(error["error"]["code"], -32602);
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn notifications_mutations_and_unimplemented_evidence_never_dispatch_reads() {
    let mut dispatch = new(limits());
    init(&mut dispatch);
    for method in ["tools/call", "tools/list", "ping", "clear_graph"] {
        assert_eq!(dispatch.receive(json!({"jsonrpc":"2.0","method":method,"params":{"name":"cartograph_read_artifact","arguments":artifact_args(&dispatch)}})),DispatchAction::Ignore);
    }
    for (index, tool) in [
        "clear_graph",
        "cartograph_evidence",
        "start_investigation",
        "sql",
    ]
    .into_iter()
    .enumerate()
    {
        let error = call(&mut dispatch, index + 1, tool, json!({}));
        assert_eq!(error["error"]["code"], -32602);
    }
    assert_eq!(
        respond(dispatch.receive(rpc(json!(9), "clear_graph", json!({}))))["error"]["code"],
        -32601
    );
    let args = identity(&dispatch);
    assert_eq!(
        call(&mut dispatch, 10, "cartograph_artifacts", args)["result"]["isError"],
        false
    );
}

#[test]
fn all_response_envelopes_and_per_call_caps_are_exactly_bounded() {
    let mut dispatch = new(limits());
    init(&mut dispatch);
    let args = artifact_args(&dispatch);
    let full = call(&mut dispatch, 1, "cartograph_read_artifact", args.clone());
    let exact = serde_json::to_vec(&full).unwrap().len();
    let mut exact_args = args.clone();
    exact_args["max_bytes"] = json!(exact);
    let exact_response = call(&mut dispatch, 2, "cartograph_read_artifact", exact_args);
    assert_eq!(exact_response["result"], full["result"]);
    assert_eq!(serde_json::to_vec(&exact_response).unwrap().len(), exact);
    let mut tight = args;
    tight["max_bytes"] = json!(exact - 1);
    let smaller = call(&mut dispatch, 3, "cartograph_read_artifact", tight);
    assert!(serde_json::to_vec(&smaller).unwrap().len() < exact);
    assert_eq!(
        smaller["result"]["structuredContent"]["artifact"]["partial"],
        true
    );
    let mut discovery = discovery_args(&dispatch);
    discovery["request"]["request"]["max_bytes"] = json!(1);
    assert_eq!(
        dispatch.receive(rpc(
            json!(4),
            "tools/call",
            json!({"name":"cartograph_discover","arguments":discovery})
        )),
        DispatchAction::Close
    );
    assert_eq!(
        dispatch.receive(rpc(json!(5), "ping", json!({}))),
        DispatchAction::Close
    );
    let mut tiny = new(DispatchLimits {
        max_response_bytes: cap(1),
        ..limits()
    });
    assert_eq!(
        tiny.receive(rpc(json!(1), "ping", json!({}))),
        DispatchAction::Close
    );
    let mut limited = new(DispatchLimits {
        max_response_bytes: cap(256),
        ..limits()
    });
    init(&mut limited);
    let list = respond(limited.receive(rpc(json!(1), "tools/list", json!({}))));
    assert_eq!(list["error"]["message"], "response_limit");
    assert!(serde_json::to_vec(&list).unwrap().len() <= 256);

    let mut discover = new(limits());
    init(&mut discover);
    let mut args = discovery_args(&discover);
    let first = call(&mut discover, 1, "cartograph_discover", args.clone());
    let exact = serde_json::to_vec(&first).unwrap().len();
    args["request"]["request"]["max_bytes"] = json!(exact);
    let boundary = call(&mut discover, 2, "cartograph_discover", args.clone());
    assert_eq!(first["result"], boundary["result"]);
    assert_eq!(serde_json::to_vec(&boundary).unwrap().len(), exact);
    args["request"]["request"]["max_bytes"] = json!(exact - 1);
    let error = call(&mut discover, 3, "cartograph_discover", args);
    assert_eq!(error["result"]["isError"], true);
    assert_eq!(
        error["result"]["structuredContent"]["error"],
        "response_limit"
    );
    assert!(serde_json::to_vec(&error).unwrap().len() < exact);

    let ping = json!({"jsonrpc":"2.0","id":1,"result":{}});
    let exact = serde_json::to_vec(&ping).unwrap().len();
    let mut ping_dispatch = new(DispatchLimits {
        max_response_bytes: cap(exact),
        ..limits()
    });
    assert_eq!(
        respond(ping_dispatch.receive(rpc(json!(1), "ping", json!({})))),
        ping
    );
    let mut too_small = new(DispatchLimits {
        max_response_bytes: cap(exact - 1),
        ..limits()
    });
    assert_eq!(
        too_small.receive(rpc(json!(1), "ping", json!({}))),
        DispatchAction::Close
    );
}

#[test]
fn limited_artifacts_preserve_stage_and_obey_client_cap() {
    let mut dispatch = new(DispatchLimits {
        trace: flowtracer::TraceLimits {
            max_output_bytes: 0,
            ..limits().trace
        },
        ..limits()
    });
    init(&mut dispatch);
    let args = artifact_args(&dispatch);
    let limited = call(&mut dispatch, 1, "cartograph_read_artifact", args.clone());
    assert_eq!(limited["result"]["isError"], true);
    assert_eq!(
        limited["result"]["structuredContent"]["limit"]["stage"],
        "trace"
    );
    assert!(!limited.to_string().contains("content_id"));
    let mut tight = args;
    let client_cap = serde_json::to_vec(&limited).unwrap().len() - 1;
    tight["max_bytes"] = json!(client_cap);
    let error = call(&mut dispatch, 2, "cartograph_read_artifact", tight);
    assert!(serde_json::to_vec(&error).unwrap().len() <= client_cap);
    assert_eq!(error["result"]["isError"], true);
    assert_eq!(
        error["result"]["structuredContent"]["error"],
        "response_limit"
    );
}

#[test]
fn security_derivation_limits_preserve_closed_stage_resource_shape() {
    for (resource, name) in [
        (spec::SecurityLimit::Rows, "rows"),
        (spec::SecurityLimit::OutputBytes, "output_bytes"),
    ] {
        assert_eq!(
            limit_value(ArtifactLimit::Compiler(
                spec::SelectedArtifactLimit::SecurityDerivation(spec::SecurityLimitExceeded {
                    limit: resource,
                    maximum: 7
                })
            )),
            json!({"stage":"security_derivation","resource":name,"maximum":7})
        );
    }
}

// AC-0595 (ADR-0058): an index whose citations do not resolve is reported
// as its own stage, never as a size limit.
#[test]
fn fact_index_failures_have_a_closed_stage_resource_shape() {
    for (failure, name) in [
        (spec::FactIndexFailure::Unresolved, "unresolved_citation"),
        (spec::FactIndexFailure::Collision, "citation_id_collision"),
        (spec::FactIndexFailure::Withheld, "withheld_citation"),
    ] {
        assert_eq!(
            limit_value(ArtifactLimit::Compiler(
                spec::SelectedArtifactLimit::FactIndex(failure)
            )),
            json!({"stage":"fact_index","resource":name})
        );
    }
}

#[test]
fn omitted_error_preserves_closed_code_and_explicit_kind() {
    for kind in [
        core_graph::source::ReceiptOmissionKind::Declarations,
        core_graph::source::ReceiptOmissionKind::Rules,
        core_graph::source::ReceiptOmissionKind::StoreCapacity,
    ] {
        let response = tool_error(&json!(1), ReadError::EvidenceOmitted(kind));
        assert_eq!(
            response["result"]["structuredContent"],
            json!({"error":"evidence_omitted", "omission_kind":kind})
        );
        assert_eq!(response["result"]["isError"], true);
    }
}

/// orders -CALLS-> payments (Confirmed) -CALLS-> ledger (InferredWeak node
/// and edge), so a verified-only session hides the ledger and its relation.
fn relations_session(mode: spec::ExportMode) -> ReadSession {
    let mut graph = SqliteGraphStore::open_in_memory().unwrap();
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
    for (id, tier) in [
        ("svc:orders", ConfidenceTier::Confirmed),
        ("svc:payments", ConfidenceTier::Confirmed),
        ("svc:ledger", ConfidenceTier::InferredWeak),
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
    ReadSession::acquire(
        &Mutex::new(graph),
        &Mutex::new(DecisionLog::open(":memory:").unwrap()),
        || Ok(vec![]),
        SnapshotReadLimits::default(),
        CoverageReadLimits::default(),
        mode,
    )
    .unwrap()
}
fn query_args(dispatch: &Dispatcher, request: Value) -> Value {
    let mut args = identity(dispatch);
    args["request"] = json!({"request":request});
    args
}
fn references(response: &Value) -> Vec<String> {
    response["result"]["structuredContent"]["context"]["response"]["facts"]
        .as_array()
        .unwrap()
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

// AC-1809 (#250): a complete session answers "what does this node relate
// to" over MCP with the in-app query contract: a node's induced
// neighborhood, only from the session's visible projection, paged under the
// issuing context and refused on a stale basis or an unknown field.
#[test]
fn query_reads_a_nodes_visible_neighborhood_with_its_relations() {
    let mut dispatch = Dispatcher::new(
        relations_session(spec::ExportMode::VerifiedOnly),
        "opaque-fixture-session".into(),
        limits(),
    )
    .unwrap();
    init(&mut dispatch);
    let capabilities = call(&mut dispatch, 1, "cartograph_capabilities", json!({}));
    let relations = &capabilities["result"]["structuredContent"]["relations_access"];
    assert_eq!(relations["via"], "cartograph_query");
    assert_eq!(relations["query_tool_available"], true);
    assert_eq!(relations["max_neighborhood_hops"], 3);

    let neighborhood = |hops: usize| json!({"scope":{"type":"neighborhood","anchor":"svc:orders","hops":hops},"max_facts":50,"max_bytes":65536});
    let args = query_args(&dispatch, neighborhood(1));
    let one = call(&mut dispatch, 2, "cartograph_query", args);
    assert_eq!(one["result"]["isError"], false, "{one}");
    assert_eq!(
        references(&one),
        [
            "svc:orders",
            "svc:payments",
            "svc:orders-CALLS->svc:payments"
        ]
    );
    // Two hops reach the ledger only where the session shows it.
    let args = query_args(&dispatch, neighborhood(2));
    let two = call(&mut dispatch, 3, "cartograph_query", args);
    assert_eq!(references(&two), references(&one));

    let mut best = Dispatcher::new(
        relations_session(spec::ExportMode::BestEffort),
        "opaque-fixture-session".into(),
        limits(),
    )
    .unwrap();
    init(&mut best);
    let args = query_args(&best, neighborhood(2));
    let all = call(&mut best, 1, "cartograph_query", args);
    assert_eq!(
        references(&all),
        [
            "svc:ledger",
            "svc:orders",
            "svc:payments",
            "svc:orders-CALLS->svc:payments",
            "svc:payments-CALLS->svc:ledger"
        ]
    );
    // Edges only, one per page, continued under the issuing context.
    let request = json!({"scope":{"type":"neighborhood","anchor":"svc:payments","hops":1},"kind":"edge","labels":["CALLS"],"max_facts":1,"max_bytes":65536});
    let args = query_args(&best, request.clone());
    let first = call(&mut best, 2, "cartograph_query", args);
    let context = &first["result"]["structuredContent"]["context"];
    assert_eq!(context["response"]["total_selected"], 2);
    assert_eq!(references(&first), ["svc:orders-CALLS->svc:payments"]);
    let mut next = query_args(&best, request);
    next["request"]["context_id"] = context["context_id"].clone();
    next["request"]["request"]["cursor"] = context["response"]["next_cursor"].clone();
    let second = call(&mut best, 3, "cartograph_query", next.clone());
    assert_eq!(references(&second), ["svc:payments-CALLS->svc:ledger"]);
    assert!(second["result"]["structuredContent"]["context"]["response"]["next_cursor"].is_null());

    // A forged basis, an unknown field, a radius past the bound and an
    // anchor the session does not show are refused.
    let mut stale = next.clone();
    stale["basis_id"] = json!("forged");
    let error = call(&mut best, 4, "cartograph_query", stale);
    assert_eq!(error["result"]["structuredContent"]["error"], "stale_basis");
    let mut unknown = next;
    unknown["request"]["request"]["sql"] = json!("DELETE FROM graph");
    let error = call(&mut best, 5, "cartograph_query", unknown);
    assert_eq!(error["error"]["code"], -32602);
    let args = query_args(&best, neighborhood(4));
    let error = call(&mut best, 6, "cartograph_query", args);
    assert_eq!(error["error"]["code"], -32602);
    let hidden = json!({"scope":{"type":"node","id":"svc:ledger"},"max_facts":1,"max_bytes":65536});
    let args = query_args(&dispatch, hidden);
    let error = call(&mut dispatch, 4, "cartograph_query", args);
    assert_eq!(error["error"]["code"], -32602);
    assert!(!error.to_string().contains("ledger"));
}

// AC-1811 (#250) worked example: an external agent answers "what handles POST
// /owners/new" purely through the MCP reads, every step citing exact facts,
// and the same reads return byte-identical responses.
#[test]
fn agent_answers_what_handles_an_endpoint_through_mcp_reads_alone() {
    let session = || {
        let mut graph = SqliteGraphStore::open_in_memory().unwrap();
        let prov = |id: &str, path: &str| {
            Provenance::new(
                Tier::Deterministic,
                ConfidenceTier::Confirmed,
                vec![EvidenceRef {
                    repo: "fixture/petclinic".into(),
                    path: path.into(),
                    byte_start: 0,
                    byte_end: 8,
                    commit_sha: "fixed".into(),
                }],
                "fixture",
                id.as_bytes(),
            )
            .unwrap()
        };
        let controller = "src/main/kotlin/OwnerController.kt";
        for (id, label, props) in [
            (
                "ep:petclinic@POST:/owners/new",
                "Endpoint",
                json!({"method":"POST","path":"/owners/new"}),
            ),
            (
                "ep:petclinic@GET:/owners/new",
                "Endpoint",
                json!({"method":"GET","path":"/owners/new"}),
            ),
            (
                "sym:OwnerController.kt#processCreationForm",
                "Symbol",
                json!({"name":"processCreationForm"}),
            ),
            (
                "sym:OwnerController.kt#initCreationForm",
                "Symbol",
                json!({"name":"initCreationForm"}),
            ),
            (
                "sym:OwnerRepository.kt#save",
                "Symbol",
                json!({"name":"save"}),
            ),
            (
                "sym:OwnerImport.kt#importOwner",
                "Symbol",
                json!({"name":"importOwner"}),
            ),
        ] {
            let mut props = props;
            props["prov"] = serde_json::to_value(prov(id, controller)).unwrap();
            graph
                .put_node(&Node {
                    id: id.into(),
                    label: label.into(),
                    props,
                })
                .unwrap();
        }
        for (src, dst, label) in [
            (
                "ep:petclinic@POST:/owners/new",
                "sym:OwnerController.kt#processCreationForm",
                "HANDLES",
            ),
            (
                "ep:petclinic@GET:/owners/new",
                "sym:OwnerController.kt#initCreationForm",
                "HANDLES",
            ),
            (
                "sym:OwnerController.kt#processCreationForm",
                "sym:OwnerRepository.kt#save",
                "CALLS",
            ),
            // A caller of the handler: the undirected neighborhood returns
            // it, and the agent must not report it as downstream work.
            (
                "sym:OwnerImport.kt#importOwner",
                "sym:OwnerController.kt#processCreationForm",
                "CALLS",
            ),
        ] {
            let id = format!("{src}>{dst}");
            graph
                .put_edge(&core_graph::Edge {
                    src: src.into(),
                    dst: dst.into(),
                    label: label.into(),
                    props: json!({"prov":prov(&id, controller)}),
                })
                .unwrap();
        }
        ReadSession::acquire(
            &Mutex::new(graph),
            &Mutex::new(DecisionLog::open(":memory:").unwrap()),
            || Ok(vec![]),
            SnapshotReadLimits::default(),
            CoverageReadLimits::default(),
            spec::ExportMode::VerifiedOnly,
        )
        .unwrap()
    };
    let answer = |dispatch: &mut Dispatcher, base: usize| {
        // 1. Find the endpoint by text; the candidate names an exact node.
        let mut args = identity(dispatch);
        args["request"] = json!({"request":{"query":{"by":"text","text":"POST:/owners/new"},"labels":["Endpoint"],"max_candidates":5,"max_bytes":65536}});
        let found = call(dispatch, base + 2, "cartograph_discover", args);
        let candidates = &found["result"]["structuredContent"]["context"]["response"]["candidates"];
        assert_eq!(candidates.as_array().unwrap().len(), 1, "{found}");
        let endpoint = candidates[0]["reference"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(endpoint, "ep:petclinic@POST:/owners/new");
        // Every answer element carries checkable provenance.
        assert_eq!(candidates[0]["confidence_tier"], "Confirmed", "{found}");
        assert!(candidates[0].to_string().contains("OwnerController.kt"));
        // 2. Follow its HANDLES relation, then what the handler calls.
        let handles = json!({"scope":{"type":"neighborhood","anchor":endpoint,"hops":1},"kind":"edge","labels":["HANDLES"],"max_facts":10,"max_bytes":65536});
        let args = query_args(dispatch, handles);
        let handled = call(dispatch, base + 3, "cartograph_query", args);
        assert_eq!(
            references(&handled),
            ["ep:petclinic@POST:/owners/new-HANDLES->sym:OwnerController.kt#processCreationForm"]
        );
        let fact = &handled["result"]["structuredContent"]["context"]["response"]["facts"][0];
        assert_eq!(fact["confidence_tier"], "Confirmed", "{fact}");
        assert!(fact.to_string().contains("OwnerController.kt"));
        let handler = fact["reference"]["destination"]
            .as_str()
            .unwrap()
            .to_owned();
        let calls = json!({"scope":{"type":"neighborhood","anchor":handler,"hops":1},"kind":"edge","labels":["CALLS"],"max_facts":10,"max_bytes":65536});
        let args = query_args(dispatch, calls);
        let called = call(dispatch, base + 4, "cartograph_query", args);
        // Neighborhoods are undirected: the page holds the handler's caller
        // too, so what the handler does is the edges whose source it is.
        assert_eq!(
            references(&called),
            [
                "sym:OwnerController.kt#processCreationForm-CALLS->sym:OwnerRepository.kt#save",
                "sym:OwnerImport.kt#importOwner-CALLS->sym:OwnerController.kt#processCreationForm"
            ]
        );
        let downstream: Vec<_> =
            called["result"]["structuredContent"]["context"]["response"]["facts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|fact| fact["reference"]["source"] == handler.as_str())
                .collect();
        assert_eq!(downstream.len(), 1);
        assert_eq!(
            downstream[0]["reference"]["destination"],
            "sym:OwnerRepository.kt#save"
        );
        assert_eq!(downstream[0]["confidence_tier"], "Confirmed");
        assert!(downstream[0].to_string().contains("OwnerController.kt"));
        // Request ids differ; the read results must not.
        [found, handled, called].map(|response| response["result"].clone())
    };
    let mut dispatch =
        Dispatcher::new(session(), "opaque-fixture-session".into(), limits()).unwrap();
    init(&mut dispatch);
    let one = answer(&mut dispatch, 0);
    // Identical reads against the same basis are byte-identical.
    let two = answer(&mut dispatch, 10);
    assert_eq!(
        serde_json::to_vec(&one).unwrap(),
        serde_json::to_vec(&two).unwrap()
    );
}
