// AC-1420, AC-1421 (ADR-0085): native tool definitions, call translation and
// the read-before-finish gate.
use super::*;
use crate::TaskFactKey;
use serde_json::json;

fn choice() -> Vec<ToolDefinition> {
    tool_definitions_from_schema(response_schema::CHOICE).unwrap()
}

fn ledger() -> InvestigationInputLedger {
    InvestigationInputLedger {
        schema_version: 2,
        coverage: None,
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: "graph:fixture".into(),
        scope_snapshot_id: "scope:fixture".into(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    }
}

fn citation(id: &str, read: bool) -> InvestigationCitation {
    InvestigationCitation {
        citation_id: id.into(),
        fact: TaskFactKey::Node {
            id: "node:a".into(),
        },
        fact_digest: "d".repeat(64),
        source: None,
        role: None,
        index: None,
        text_hash: read.then(|| "h".repeat(64)),
        origin: InvestigationEvidenceOrigin::GraphMetadata,
    }
}

fn query() -> InvestigationQueryManifest {
    InvestigationQueryManifest {
        query: serde_json::from_value(json!({"scope":{"type":"all"},"kind":null,"labels":[],
            "max_facts":12,"max_bytes":16384,"cursor":null}))
        .unwrap(),
        response_hash: "r".repeat(64),
        response_bytes: 10,
        returned_facts: 1,
        total_selected: 1,
        has_more: false,
    }
}

#[test]
fn choice_schema_becomes_the_closed_tool_vocabulary() {
    let tools = choice();
    let names: Vec<_> = tools.iter().map(|tool| tool.name).collect();
    assert_eq!(
        names,
        [
            "query_context",
            "read_evidence",
            "read_coverage_inventory",
            "replay_query",
            "finish"
        ]
    );
    for tool in &tools {
        assert!(AgentTool::from_name(tool.name).is_some());
        assert!(
            tool.input_schema["$defs"].is_object(),
            "{} keeps $defs",
            tool.name
        );
        if tool.name != "finish" {
            assert!(tool.input_schema["properties"].get("type").is_none());
            assert!(tool.input_schema["properties"].get("context_id").is_none());
            let required = tool.input_schema["required"].as_array().unwrap();
            assert!(required.iter().all(|f| f != "type" && f != "context_id"));
        }
    }
    let host_context: Vec<_> = tools
        .iter()
        .filter(|tool| tool.host_context_id)
        .map(|tool| tool.name)
        .collect();
    assert_eq!(
        host_context,
        ["query_context", "read_coverage_inventory", "replay_query"]
    );
    let finish = tools.iter().find(|tool| tool.name == "finish").unwrap();
    assert_eq!(finish.input_schema["anyOf"].as_array().unwrap().len(), 2);
}

#[test]
fn role_narrowing_carries_into_the_tools() {
    let writer = tool_definitions_from_schema(
        response_schema::select_completion(SpecialistId::SectionWriterV1)
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert!(writer.iter().all(|tool| tool.name != "replay_query"));
    assert!(
        !serde_json::to_string(&writer)
            .unwrap()
            .contains("proposed_design")
    );
}

#[test]
fn a_tool_call_decodes_exactly_like_the_bounded_action() {
    let tools = choice();
    let ctx = format!("context-v2:{}", "a".repeat(64));
    let ctx = ctx.as_str();
    let query = json!({"scope":{"type":"all"},"kind":null,"labels":[],"max_facts":12,
        "max_bytes":16384,"cursor":null});
    let raw =
        action_from_tool_call(&tools, "query_context", &json!({ "query": query }), ctx).unwrap();
    let bounded = json!({"type":"query_context","context_id":ctx,"query":query}).to_string();
    assert_eq!(
        serde_json::from_str::<Value>(&raw).unwrap(),
        serde_json::from_str::<Value>(&bounded).unwrap()
    );
    let from_tool = CompletionInvestigationAction::decode_detailed(&raw, ctx)
        .map_err(|failure| failure.error)
        .unwrap();
    let from_json = CompletionInvestigationAction::decode_detailed(&bounded, ctx)
        .map_err(|failure| failure.error)
        .unwrap();
    assert_eq!(format!("{from_tool:?}"), format!("{from_json:?}"));

    let read = action_from_tool_call(
        &tools,
        "read_evidence",
        &json!({"fact":{"kind":"node","id":"node:a"},"role":"provenance","index":0}),
        ctx,
    )
    .unwrap();
    assert!(
        !read.contains("context_id"),
        "read_evidence never carried context_id"
    );
}

#[test]
fn bad_calls_are_refusals_with_prescriptions() {
    let tools = choice();
    let unknown = action_from_tool_call(&tools, "bash", &json!({}), "ctx:1").unwrap_err();
    assert_eq!(unknown.code, "unknown_tool");
    assert!(unknown.prescription.contains("query_context"));
    assert_eq!(
        action_from_tool_call(&tools, "query_context", &json!("all"), "ctx:1")
            .unwrap_err()
            .code,
        "invalid_arguments"
    );
    for field in ["type", "context_id"] {
        let mut args = Map::new();
        args.insert(field.into(), json!("x"));
        assert_eq!(
            action_from_tool_call(&tools, "query_context", &Value::Object(args), "ctx:1")
                .unwrap_err()
                .code,
            "host_owned_field"
        );
    }
    let writer = tool_definitions_from_schema(
        response_schema::select_completion(SpecialistId::SectionWriterV1)
            .unwrap()
            .as_str(),
    )
    .unwrap();
    assert_eq!(
        action_from_tool_call(&writer, "replay_query", &json!({}), "ctx:1")
            .unwrap_err()
            .code,
        "unknown_tool",
        "a tool the role was not offered is refused"
    );
}

#[test]
fn finish_is_wrapped_by_the_host() {
    let tools = choice();
    let supported = action_from_tool_call(
        &tools,
        "finish",
        &json!({"findings":[{"claim_kind":"inferred_interpretation","title":"t","statement":"s",
            "citation_ids":["c"],"limitations":["l"]}],"knowledge_completeness":"partial","limitations":["l"]}),
        "ctx:1",
    )
    .unwrap();
    let value: Value = serde_json::from_str(&supported).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["outcome"], "supported");
    assert_eq!(value["action"]["type"], "finish");
    assert!(value["action"].get("context_id").is_none());
    let insufficient = action_from_tool_call(
        &tools,
        "finish",
        &json!({"findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["l"]}),
        "ctx:1",
    )
    .unwrap();
    assert!(insufficient.contains("\"outcome\":\"insufficient_evidence\""));
    let ctx = format!("context-v2:{}", "a".repeat(64));
    for raw in [&supported, &insufficient] {
        let decoded = CompletionInvestigationAction::decode_detailed(raw, &ctx)
            .map_err(|failure| failure.error)
            .unwrap();
        assert!(matches!(decoded, CompletionInvestigationAction::Finish(_)));
    }
    // GLM on vLLM sent the finish's findings array as its JSON text (9851e062
    // smoke); the finish variants declare it an array, so it decodes back.
    let findings = json!([{"claim_kind":"inferred_interpretation","title":"t","statement":"s",
        "citation_ids":["c"],"limitations":["l"]}]);
    let stringified = action_from_tool_call(
        &tools,
        "finish",
        &json!({"findings": findings.to_string(), "knowledge_completeness": "partial",
            "limitations": json!(["l"]).to_string()}),
        "ctx:1",
    )
    .unwrap();
    assert_eq!(stringified, supported);
}

#[test]
fn finish_waits_for_a_source_read_and_acquired_citations() {
    let finding = |ids: Value| {
        json!({"findings":[{"claim_kind":"implemented_behavior","title":"t","statement":"s",
            "citation_ids":ids,"limitations":["l"]}],"knowledge_completeness":"partial","limitations":["l"]})
    };
    let mut current = ledger();
    current.citations.push(citation("fact:a", false));
    let refused = read_before_finish(&current, &finding(json!(["fact:a"]))).unwrap_err();
    assert_eq!(refused.code, "read_before_finish");
    assert!(refused.prescription.contains("read_evidence"));

    current.citations.push(citation("source:a", true));
    let unacquired =
        read_before_finish(&current, &finding(json!(["source:a", "source:zz"]))).unwrap_err();
    assert!(unacquired.prescription.contains("source:zz"));
    assert!(!unacquired.prescription.contains("source:a,"));
    read_before_finish(&current, &finding(json!(["fact:a", "source:a"]))).unwrap();
}

#[test]
fn insufficient_evidence_needs_one_query_first() {
    let insufficient =
        json!({"findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["l"]});
    let mut current = ledger();
    assert_eq!(
        read_before_finish(&current, &insufficient)
            .unwrap_err()
            .code,
        "read_before_finish"
    );
    current.queries.push(query());
    read_before_finish(&current, &insufficient).unwrap();
}
