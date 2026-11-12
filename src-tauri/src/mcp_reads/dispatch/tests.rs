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
fn lifecycle_and_real_schemas_advertise_only_four_implemented_reads() {
    let mut dispatch = new(limits());
    let before = respond(dispatch.receive(rpc(json!(0), "tools/list", json!({}))));
    assert!(before.get("error").is_some());
    init(&mut dispatch);
    let response = respond(dispatch.receive(rpc(json!(1), "tools/list", json!({}))));
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
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
    assert_eq!(data["h3_curated_available"], false);
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
        spec::artifact_catalog().len()
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
