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
    finish_action(prompt)
}

fn finish_action(prompt: &Value) -> Value {
    let citations = prompt["citations"].as_array().unwrap();
    json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{
        "claim_kind":"inferred_interpretation","title":"Seven distinct retained source selections",
        "statement":"The selected callables and local declaration were acquired as bounded source evidence.",
        "citation_ids":citations.iter().filter(|c|c["origin"]["kind"]=="captured_primary_source").map(|c|c["citation_id"].clone()).collect::<Vec<_>>(),
        "limitations":["This mechanical fixture does not establish runtime behavior."]}],
        "knowledge_completeness":"partial","limitations":["Only the caller-selected source set was inspected."]}})
}

// AC-0489: choose the exact supplied finite object; never reconstruct query,
// range, receipt or inventory authority from descriptive obligation fields.
pub(super) fn next_offered_action(prompt: &Value, schema: &Value) -> Value {
    assert!(
        prompt["turn_progress"]["capacity_feedback"].is_null(),
        "capacity_feedback={} mode={} usage={} choices={}",
        prompt["turn_progress"]["capacity_feedback"],
        prompt["turn_progress"]["admission_mode"],
        prompt["consumed_budgets"],
        prompt["turn_progress"]["acquisition_choices"],
    );
    assert!(prompt["turn_progress"]["action_feedback"].is_null());
    let choices = &prompt["turn_progress"]["acquisition_choices"];
    if let Some(entry) = choices["entries"].as_array().unwrap().first() {
        let action = entry["action"].clone();
        let finite = schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|branch| branch.get("enum"))
            .unwrap()
            .as_array()
            .unwrap();
        assert!(finite.contains(&action));
        return action;
    }
    assert!(choices["blocked"].as_array().unwrap().is_empty());
    assert_eq!(choices["deferred"], 0);
    finish_action(prompt)
}

#[test]
fn exact_node_worker_acquires_seven_mixed_targets_inventory_finishes_and_reopens() {
    let mut scripted = NextProvider::new(false);
    scripted.exact_targets = true;
    let provider = Arc::new(scripted);
    let (base, request) = seven_target_base(provider.as_ref());
    let fixture = NextFixture::from_completion_base_with_wall_bounded_active(
        provider.as_ref(),
        SpecialistId::DomainAnalystV17,
        base,
        request,
    );
    fixture.run(provider.clone());
    assert_seven_completed(&fixture, provider.as_ref());
}

fn seven_target_base(provider: &dyn LlmProvider) -> (CompletionFixture, StagedObligationRequest) {
    let source = (0..6).map(|i| format!("export function exact_{i}(input: {{ enabled: boolean }}) {{ const {} = input.enabled; if ({} === false) return false; return true; }}\n",
        if i==0 {"exactFlag"} else {"guard"}, if i==0 {"exactFlag"} else {"guard"})).collect::<String>();
    // Only oversized callable interiors emit direct local Declaration facts.
    let source = format!(
        "{source}export function oversized() {{ /* {} */ const exactFlag = true; return exactFlag; }}\n",
        "fixture comment ".repeat(600)
    );
    let base = CompletionFixture::with_original(
        provider,
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
    (base, request)
}

fn assert_seven_completed(fixture: &NextFixture, provider: &NextProvider) {
    let state = fixture.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "error={:?}; usage={:?}; limits={:?}; provider_calls={}",
        detail.error,
        detail.usage,
        detail.limits,
        provider.observed.calls.load(Ordering::SeqCst)
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

// AC-0489: compose all seven real retained source acquisitions with the gated
// initial writer/private worker, preserving public/store/run/resume closure.
#[test]
fn gated_exact_node_worker_selects_seven_offered_targets_and_reopens() {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(GatedFlow::OfferedTargets);
    let provider = Arc::new(scripted);
    let (base, request) = seven_target_base(provider.as_ref());
    let fixture = NextFixture::gated_from_completion_base(provider.as_ref(), base, request);
    let state = fixture.base.base.app.state::<AppState>();
    let before = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    assert_eq!(before.summary.specialist_id, SpecialistId::DomainAnalystV18);
    assert_eq!(before.usage.model_invocations, 0);
    // Real wall clock, synthetic active time (AC-0287). The gated rewrite
    // keeps the 600 s default allowance, and 16 debug-build host turns on a
    // slow shared runner spend all of it (#1896); the real active clock stays
    // covered by the @17 and @20 runs above and below on a wider allowance.
    fixture.run_gated_inner_frozen_active(provider.as_ref());
    assert_seven_completed(&fixture, provider.as_ref());
    let prompts = provider.observed.prompts.lock().unwrap();
    let payloads = provider.received_payloads.lock().unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 16);
    assert_eq!(prompts.len(), 16);
    assert_eq!(payloads.len(), prompts.len());
    assert_eq!(prompts[0]["consumed_budgets"]["model_invocations"], 0);
    assert_eq!(
        prompts[0]["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    for (ordinal, (prompt, payload)) in prompts.iter().zip(payloads.iter()).enumerate() {
        assert_eq!(payload.system, SpecialistId::DomainAnalystV18.prompt());
        assert_eq!(prompt["consumed_budgets"]["model_invocations"], ordinal);
        let schema: Value =
            serde_json::from_str(payload.response_schema.as_deref().unwrap()).unwrap();
        let action = next_offered_action(prompt, &schema);
        if ordinal < 15 {
            assert!(
                prompt["turn_progress"]["acquisition_choices"]["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["action"] == action)
            );
            if action["type"] == "query_context" {
                assert_eq!(action["query"]["scope"]["type"], "node");
                assert_eq!(action["query"]["max_facts"], 1);
            }
        } else {
            assert_eq!(action["action"]["type"], "finish");
        }
    }
    let connection =
        rusqlite::Connection::open(fixture.base.base.app_data.join("state.db")).unwrap();
    let invocations: i64 = connection
        .query_row(
            "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        usize::try_from(invocations).unwrap(),
        provider.observed.calls.load(Ordering::SeqCst)
    );
}

// AC-0566: production @20 with seven source obligations plus inventory must
// acquire before finishing, including the exact first provider request.
#[test]
fn analyst20_worker_eight_obligations_offer_acquisition_then_finish() {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(GatedFlow::OfferedTargets);
    let provider = Arc::new(scripted);
    let (base, request) = seven_target_base(provider.as_ref());
    let fixture = NextFixture::from_completion_base_with_wall_bounded_active(
        provider.as_ref(),
        SpecialistId::DomainAnalystV20,
        base,
        request,
    );
    fixture.run(provider.clone());
    assert_seven_completed(&fixture, provider.as_ref());
    let prompts = provider.observed.prompts.lock().unwrap();
    let payloads = provider.received_payloads.lock().unwrap();
    assert_eq!(
        prompts[0]["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert_eq!(payloads.len(), 16);
    for (ordinal, payload) in payloads.iter().enumerate() {
        assert_eq!(payload.system, SpecialistId::DomainAnalystV20.prompt());
        let schema: Value =
            serde_json::from_str(payload.response_schema.as_deref().unwrap()).unwrap();
        let branches = schema["anyOf"].as_array().unwrap();
        if ordinal < 15 {
            assert_eq!(branches.len(), 1);
            assert!(branches[0].get("$ref").is_none());
            let offered = branches[0]["enum"].as_array().unwrap();
            assert!(!offered.is_empty());
            assert!(offered.contains(&next_offered_action(&prompts[ordinal], &schema)));
        } else {
            assert_eq!(branches.len(), 2);
            assert!(
                branches.iter().all(|branch| matches!(
                    branch["$ref"].as_str(),
                    Some("#/$defs/supported_wrapper" | "#/$defs/insufficient_wrapper")
                )),
                "all eight acquisitions must close exploration without an extra query"
            );
            assert!(
                branches
                    .iter()
                    .any(|b| b["$ref"] == "#/$defs/supported_wrapper")
            );
            let ids = schema["$defs"]["finding"]["properties"]["citation_ids"]["items"]["enum"]
                .as_array()
                .unwrap();
            assert_eq!(
                ids.len(),
                prompts[ordinal]["citations"].as_array().unwrap().len()
            );
            assert!(ids.iter().all(|id| {
                prompts[ordinal]["citations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["citation_id"] == *id)
            }));
        }
    }
}

// AC-1433: rule-bearing suggestions name captured callables that own recovered
// guarded exits, spread at most two per file while other files have owners,
// and each resolves as a source obligation.
#[test]
fn rule_bearing_sources_name_resolvable_rule_owners_capped_per_file() {
    let provider = NextProvider::new(false);
    let (base, _) = seven_target_base(&provider);
    let state = base.base.app.state::<AppState>();
    let sources =
        crate::investigations::rule_sources::rule_bearing_sources(&state, None, None, 7).unwrap();
    // AC-1442: the cap spreads suggestions across files, and this fixture has
    // one file, so its further owners fill the slots the cap left empty,
    // after the two the cap admitted.
    let capped =
        crate::investigations::rule_sources::rule_bearing_sources(&state, None, None, 2).unwrap();
    assert_eq!(capped.len(), 2);
    assert!(sources.len() > capped.len(), "{sources:?}");
    for (first, same) in capped.iter().zip(&sources) {
        assert_eq!(first.fact, same.fact);
    }
    let distinct: std::collections::BTreeSet<_> =
        sources.iter().map(|source| &source.fact).collect();
    assert_eq!(distinct.len(), sources.len(), "no owner is offered twice");
    for source in &sources {
        assert!(source.owner.guarded_exits >= 1);
        let agents::TaskFactKey::Node { id } = &source.fact else {
            panic!("owners are node facts");
        };
        assert!(id.contains("#exact_") || id.contains("#oversized"), "{id}");
        crate::investigations::inspection::resolve(
            &state,
            &source.fact,
            &source.graph_snapshot_id,
            &source.receipt_id,
            source.range_index,
        )
        .unwrap();
    }
    let elsewhere: std::collections::BTreeSet<String> = ["no/such/file.ts".to_owned()].into();
    assert!(
        crate::investigations::rule_sources::rule_bearing_sources(
            &state,
            None,
            Some(&elsewhere),
            7
        )
        .unwrap()
        .is_empty()
    );
}
