//! AC-0461: real retained mixed-source acquisition, with no network provider.
use super::*;

pub(super) fn next_action(prompt: &Value) -> Value {
    assert!(
        prompt["turn_progress"]["capacity_feedback"].is_null(),
        "revision={} feedback={} usage={}",
        prompt["turn_progress"]["input_revision"],
        prompt["turn_progress"]["capacity_feedback"],
        prompt["consumed_budgets"]
    );
    let obligations = prompt["completion_contract"]["obligations"]["obligations"]
        .as_array()
        .unwrap();
    let citations = prompt["citations"].as_array().unwrap();
    if let Some(item) = obligations
        .iter()
        .filter(|o| o["kind"] == "source")
        .find(|o| {
            !citations.iter().any(|c| {
                c["origin"]["kind"] == "captured_primary_source"
                    && c["fact"] == o["target"]["fact"]
                    && c["role"] == o["target"]["role"]
                    && c["index"] == o["target"]["index"]
            })
        })
    {
        let target = &item["target"];
        if let Some(option) = prompt["compact_context"]["evidence_options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["fact"] == target["fact"])
        {
            assert!(
                option["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["role"] == target["role"]
                        && r["index"] == target["index"]
                        && r["source"] == target["source"])
            );
            return json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]});
        }
        return json!({"type":"query_context","context_id":prompt["context_id"],"query":{
            "scope":{"type":"node","id":target["fact"]["id"]},"kind":"node","labels":[],"max_facts":32,"max_bytes":32768,"cursor":null}});
    }
    if prompt["turn_progress"]["last_completed_action"]["type"] != "read_coverage_inventory" {
        let target = &obligations
            .iter()
            .find(|o| o["kind"] == "inventory")
            .unwrap()["target"];
        return json!({"type":"read_coverage_inventory","context_id":target["context_id"],"repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768});
    }
    json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{
        "claim_kind":"inferred_interpretation","title":"Seven distinct retained source selections",
        "statement":"The selected callables and local declaration were acquired as bounded source evidence.",
        "citation_ids":citations.iter().filter(|c|c["origin"]["kind"]=="captured_primary_source").map(|c|c["citation_id"].clone()).collect::<Vec<_>>(),
        "limitations":["This mechanical fixture does not establish runtime behavior."]}],
        "knowledge_completeness":"partial","limitations":["Only the caller-selected source set was inspected."]}})
}

#[test]
fn exact_node_worker_acquires_seven_mixed_targets_inventory_finishes_and_reopens() {
    let mut scripted = NextProvider::new(false);
    scripted.exact_targets = true;
    let provider = Arc::new(scripted);
    let source = (0..6).map(|i| format!("export function exact_{i}(input: {{ enabled: boolean }}) {{ const {} = input.enabled; if ({} === false) return false; return true; }}\n",
        if i==0 {"exactFlag"} else {"guard"}, if i==0 {"exactFlag"} else {"guard"})).collect::<String>();
    // Only oversized callable interiors emit direct local Declaration facts.
    let source = format!(
        "{source}export function oversized() {{ /* {} */ const exactFlag = true; return exactFlag; }}\n",
        "fixture comment ".repeat(600)
    );
    let base = CompletionFixture::with_original(
        provider.as_ref(),
        SpecialistId::DomainAnalystV9,
        false,
        true,
        &source,
    );
    let state = base.base.app.state::<AppState>();
    let nodes = state.graph.lock().unwrap().all_nodes().unwrap();
    let mut ids = nodes
        .iter()
        .filter(|n| n.label == "Symbol" && (0..6).any(|i| n.id.ends_with(&format!("#exact_{i}"))))
        .map(|n| n.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 6);
    ids.push(
        nodes
            .iter()
            .find(|n| n.label == "Declaration" && n.props["name"] == "oversized/exactFlag")
            .expect("retained local declaration")
            .id
            .clone(),
    );
    assert!(nodes.iter().filter(|n| n.label == "BusinessRule").count() >= 6);
    let graph = state.graph.lock().unwrap().read_snapshot().unwrap();
    let snapshot = context_hub::ContextSnapshot::new(graph.0, graph.1).unwrap();
    let anchor = ids[0].clone();
    let exact = snapshot
        .query(context_hub::QueryRequest {
            scope: context_hub::QueryScope::Node { id: anchor.clone() },
            ..Default::default()
        })
        .unwrap();
    let neighbors = snapshot
        .query(context_hub::QueryRequest {
            scope: context_hub::QueryScope::Neighborhood { anchor, hops: 1 },
            kind: Some(context_hub::FactKind::Node),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(exact.facts.len(), 1);
    assert!(neighbors.facts.len() > 1);
    assert!(
        serde_json::to_vec(&neighbors).unwrap().len() > serde_json::to_vec(&exact).unwrap().len()
    );
    let mut request = base.request.clone();
    request.expected_graph_revision = Some(snapshot.id().to_owned());
    for id in ids {
        let fact = agents::TaskFactKey::Node { id };
        let description =
            crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact))
                .unwrap();
        let (target, _guard) = crate::investigations::inspection::resolve(
            &state,
            &fact,
            &description.graph_snapshot_id,
            &description.receipt_id,
            0,
        )
        .unwrap();
        let mut item = Obligation::Source {
            obligation_id: String::new(),
            target: Box::new(target),
        };
        let hash = item.content_id().unwrap();
        if let Obligation::Source { obligation_id, .. } = &mut item {
            *obligation_id = hash;
        }
        request.obligations.obligations.push(item);
    }
    request.obligations = request
        .obligations
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let fixture = NextFixture::from_completion_base(
        provider.as_ref(),
        SpecialistId::DomainAnalystV17,
        base,
        request,
    );
    fixture.run(provider.clone());
    let state = fixture.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::Supported)
    );
    assert_eq!(
        (
            detail.usage.model_invocations,
            detail.usage.tool_actions,
            detail.usage.evidence_items
        ),
        (16, 15, 7)
    );
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(result.completion.as_ref().unwrap().assessment.len(), 8);
    assert!(
        result
            .completion
            .as_ref()
            .unwrap()
            .assessment
            .iter()
            .all(|a| a.attempted && a.acquired && a.exactly_supplied && a.unmet.is_none())
    );
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 7);
    assert_eq!(ledger.coverage_reads.len(), 1);
    assert!(ledger.queries.iter().all(|q| matches!(
        q.query.scope,
        InvestigationScope::Node { .. }
    ) && q.returned_facts == 1
        && q.total_selected == 1
        && !q.has_more));
    for item in &fixture.request.obligations.obligations {
        if let Obligation::Source { target, .. } = item {
            let c = ledger
                .citations
                .iter()
                .find(|c| {
                    c.fact == target.fact
                        && matches!(
                            c.origin,
                            InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
                        )
                })
                .unwrap();
            assert_eq!(
                (&c.source, c.role, c.index),
                (
                    &Some(target.source.clone()),
                    Some(target.role),
                    Some(target.index)
                )
            );
            let InvestigationEvidenceOrigin::CapturedPrimarySource {
                registered_source_id,
                receipt_id,
                receipt_inventory_index,
                captured,
                ..
            } = &c.origin
            else {
                unreachable!()
            };
            assert_eq!(
                (
                    registered_source_id,
                    receipt_id,
                    *receipt_inventory_index,
                    captured
                ),
                (
                    &target.registered_source_id,
                    &target.receipt_id,
                    target.receipt_inventory_index,
                    &target.captured
                )
            );
        }
    }
    let reopened =
        crate::jobs::JobStore::open(fixture.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap().unwrap(),
        result
    );
    assert_eq!(
        reopened.investigation_ledger(&fixture.id).unwrap().unwrap(),
        ledger
    );
    assert_eq!(
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
        fixture.base.base.graph_before
    );
}
