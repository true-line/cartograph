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

// AC-0566: retained DEVELOPMENT response, never a live provider or acceptance claim.
pub(super) fn source_finish_action(prompts: &[Value], call: usize, paraphrase: bool) -> Value {
    if call < 2 {
        return prompts[call]["turn_progress"]["acquisition_choices"]["entries"][0]["action"]
            .clone();
    }
    if call == 2 {
        return prompts[0]["turn_progress"]["acquisition_choices"]["entries"][0]["action"].clone();
    }
    assert_eq!(call, 3);
    assert_eq!(
        prompts[call]["turn_progress"]["admission_mode"],
        "finish_only"
    );
    let mut value: Value =
        serde_json::from_str(include_str!("retained_smoke_finish.json")).unwrap();
    if paraphrase {
        value["action"]["findings"][0]["statement"] =
            json!("The acquired callable includes a conditional exit.");
    }
    value
}

#[test]
fn analyst20_retained_finish_after_duplicate_source_read() {
    for paraphrase in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(if paraphrase {
            GatedFlow::ParaphrasedSourceFinish
        } else {
            GatedFlow::RetainedSourceFinish
        });
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV20,
            true,
        );
        f.run(provider.clone());
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        let detail = reopened.investigation(&f.id).unwrap();
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
        assert_eq!(detail.usage.evidence_items, 1);
        let payloads = provider.received_payloads.lock().unwrap();
        let source = &payloads[3].spans[0].text;
        assert_eq!(source, "ok === false");
        let prompts = provider.observed.prompts.lock().unwrap();
        let value = source_finish_action(&prompts, 3, paraphrase);
        let statement = value["action"]["findings"][0]["statement"]
            .as_str()
            .unwrap();
        let replay = agents::investigation::reject_prose_replay([statement], [source.as_str()]);
        assert_eq!(
            replay,
            if paraphrase {
                Ok(())
            } else {
                Err(InvestigationError::SourceReplay)
            }
        );
        assert_eq!(
            detail.summary.status,
            if paraphrase {
                InvestigationStatus::Completed
            } else {
                InvestigationStatus::Failed
            },
            "paraphrase={paraphrase}: {:?}",
            detail.error
        );
        assert_eq!(
            reopened.investigation_result(&f.id).unwrap().is_some(),
            paraphrase
        );
        let events = reopened.investigation_events(&f.id, 0).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == InvestigationEventKind::ModelCompleted)
                .count(),
            4
        );
        assert!(detail.usage.reported_input_tokens.is_some());
        assert!(detail.usage.reported_output_tokens.is_some());
        assert!(
            !events
                .items
                .iter()
                .any(|e| e.summary.contains("ok === false"))
        );
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        assert_eq!(jobs.investigation(&f.id).unwrap(), detail);
        assert_eq!(jobs.investigation_events(&f.id, 0).unwrap(), events);
        if !paraphrase {
            assert_eq!(
                events
                    .items
                    .iter()
                    .filter(|e| e.kind == InvestigationEventKind::FinishRefused)
                    .count(),
                1
            );
            assert_eq!(
                detail.error.as_deref(),
                Some("Investigation finish refused: source_replay.")
            );
        }
    }
}

// AC-0566: malformed authored prose uses the same once-only refusal allowance.
pub(super) fn prose_action(prompt: &Value, call: usize, exhausted: bool) -> Value {
    if exhausted && call < 3 {
        return next_action(prompt, call, false);
    }
    if !exhausted && call < 2 {
        return prompt["turn_progress"]["acquisition_choices"]["entries"][0]["action"].clone();
    }
    let mut response = next_action(prompt, call + 1, false);
    if exhausted || call == 2 {
        response["action"]["limitations"] =
            json!(["action_placeholder_must_not_be_used_here_but_action_must_be_finish}],"]);
    } else {
        assert_eq!(call, 3);
        assert_eq!(
            prompt["completion_metadata"]["feedback"]["prose_correction"],
            "rewrite_schema_placeholders_as_plain_prose"
        );
        assert_eq!(
            prompt["completion_metadata"]["feedback"]["schema_version"],
            3
        );
    }
    response
}

#[test]
fn analyst20_prose_refusal_precedes_audit_and_shares_correction_allowance() {
    for exhausted in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(if exhausted {
            GatedFlow::ExhaustedProseCorrection
        } else {
            GatedFlow::ProseCorrection
        });
        scripted.audit_verdict = Some("pass");
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV20,
            true,
        );
        f.run_with_audit_options(provider.as_ref(), RunClock::new(Instant::now()), None, true);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        let detail = reopened.investigation(&f.id).unwrap();
        assert!(!detail.summary.invocation_pending);
        assert_eq!(detail.summary.has_result, !exhausted, "{:?}", detail.error);
        if exhausted {
            assert_eq!(
                detail.error.as_deref(),
                Some(
                    "The answer still contained incomplete or placeholder text after one correction. No findings were saved."
                )
            );
        } else {
            assert!(detail.error.is_none());
        }
        assert_eq!(
            provider.observed.calls.load(Ordering::SeqCst),
            if exhausted { 4 } else { 5 }
        );
        assert_eq!(
            detail.usage.model_invocations,
            if exhausted { 4 } else { 5 }
        );
        assert_eq!(
            detail.summary.status,
            if exhausted {
                InvestigationStatus::Failed
            } else {
                InvestigationStatus::Completed
            },
            "{:?}",
            detail.error
        );
        let events = reopened.investigation_events(&f.id, 0).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == InvestigationEventKind::FinishRefused)
                .count(),
            if exhausted { 2 } else { 1 }
        );
        let state = f.base.base.app.state::<AppState>();
        assert_eq!(
            state.jobs.lock().unwrap().investigation(&f.id).unwrap(),
            detail
        );
    }
}
