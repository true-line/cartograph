//! AC-0566: real worker, retained synthetic source, typed correction and result reopen.
use super::*;

pub(super) fn next_action(prompt: &Value, call: usize, receipt: bool) -> Value {
    let target = &prompt["completion_contract"]["obligations"]["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "source")
        .unwrap()["target"];
    if call == 0 {
        return finish(target[if receipt { "receipt_id" } else { "target_id" }].clone());
    }
    if let Some(entry) = prompt["turn_progress"]["citation_correction"]["entries"]
        .as_array()
        .and_then(|entries| entries.first())
    {
        return entry["action"].clone();
    }
    if let Some(entry) = prompt["turn_progress"]["acquisition_choices"]["entries"]
        .as_array()
        .and_then(|entries| entries.first())
    {
        return entry["action"].clone();
    }
    let citation = prompt["citations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["origin"]["kind"] == "captured_primary_source")
        .unwrap();
    assert_ne!(citation["citation_id"], target["target_id"]);
    finish(citation["citation_id"].clone())
}
fn finish(id: Value) -> Value {
    json!({"schema_version":1,"outcome":"supported","action":{
        "type":"finish","findings":[{"claim_kind":"inferred_interpretation",
        "title":"Selected guard","statement":"The acquired callable includes a conditional exit.",
        "citation_ids":[id],"limitations":["Runtime inputs remain unknown."]}],
        "knowledge_completeness":"partial","limitations":["No complete business policy was established."]}})
}

#[test]
fn analyst20_worker_correction_queries_reads_and_reopens_supported_result() {
    correction_round_trip(GatedFlow::TypedCorrection);
}

#[test]
fn analyst20_worker_receipt_correction_queries_reads_and_reopens_supported_result() {
    correction_round_trip(GatedFlow::ReceiptCorrection);
}

fn correction_round_trip(flow: GatedFlow) {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(flow);
    let provider = Arc::new(scripted);
    let f = NextFixture::with_source_obligation(
        provider.as_ref(),
        SpecialistId::DomainAnalystV20,
        true,
    );
    f.run(provider.clone());
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
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
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 1);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(detail.usage.evidence_items, 1);
    assert!(reopened.investigation_result(&f.id).unwrap().is_some());
    let prompts = provider.observed.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 4);
    assert_eq!(detail.usage.model_invocations, 4);
    assert_eq!(detail.usage.tool_actions, 2);
    assert_eq!(
        prompts[1]["turn_progress"]["citation_correction"]["entries"][0]["action"]["type"],
        "query_context"
    );
    assert_eq!(
        prompts[2]["turn_progress"]["citation_correction"]["entries"][0]["action"]["type"],
        "read_evidence"
    );
    assert!(prompts[3]["turn_progress"]["citation_correction"].is_null());
}
