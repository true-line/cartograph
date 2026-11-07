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
    assert!(matches!(call, 3 | 4));
    if call == 4 {
        assert_eq!(
            prompts[call]["completion_metadata"]["feedback"]["prose_correction"],
            "paraphrase_without_quoting_source"
        );
    }
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
        let expected_calls = if paraphrase { 4 } else { 5 };
        assert_eq!(
            provider.observed.calls.load(Ordering::SeqCst),
            expected_calls
        );
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
            expected_calls
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
                2
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

pub(super) fn replay_action(prompt: &Value, call: usize, flow: GatedFlow) -> Value {
    if flow == GatedFlow::SpentReplay && call < 3 {
        return next_action(prompt, call, false);
    }
    if call < 2 {
        return prompt["turn_progress"]["acquisition_choices"]["entries"][0]["action"].clone();
    }
    let mut response = next_action(prompt, call, false);
    if call == 3 && flow != GatedFlow::SpentReplay {
        assert_eq!(
            prompt["completion_metadata"]["feedback"],
            json!({
                "schema_version":3, "unmet":[], "prose_correction":"paraphrase_without_quoting_source"
            })
        );
        if flow == GatedFlow::RepairReplay {
            return response;
        }
    }
    response["action"]["findings"][0]["statement"] = json!("The guard quotes ok === false here.");
    response
}

#[test]
fn analyst20_source_replay_shares_correction_and_requires_audit() {
    for flow in [
        GatedFlow::RepairReplay,
        GatedFlow::RepeatReplay,
        GatedFlow::SpentReplay,
    ] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(flow);
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
        let repaired = flow == GatedFlow::RepairReplay;
        assert_eq!(detail.summary.has_result, repaired, "{:?}", detail.error);
        assert_eq!(
            reopened.investigation_result(&f.id).unwrap().is_some(),
            repaired
        );
        assert_eq!(detail.usage.model_invocations, if repaired { 5 } else { 4 });
        assert_eq!(
            detail.error.as_deref(),
            (!repaired).then_some("Investigation finish refused: source_replay.")
        );
    }
}

#[test]
fn analyst20_denied_source_stops_after_one_correction_and_reopens() {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::RepeatDeniedEvidence);
    let f = NextFixture::with_source_obligation(&provider, SpecialistId::DomainAnalystV20, true);
    f.run_with_audit_options(&provider, RunClock::new(Instant::now()), None, true);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
    assert_eq!(
        detail.error.as_deref(),
        Some("The corrected answer still denied acquired evidence. No findings were saved.")
    );
    assert_eq!(detail.usage.model_invocations, 4);
    assert!(reopened.investigation_result(&f.id).unwrap().is_none());
}

// AC-0287: two genuine acquired-source finishes share the single correction.
pub(super) fn partial_action(prompt: &Value, call: usize) -> Value {
    if call < 2 {
        let kind = if call == 0 {
            "query_context"
        } else {
            "read_evidence"
        };
        return prompt["turn_progress"]["acquisition_choices"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["action"]["type"] == kind)
            .expect("source acquisition choice")["action"]
            .clone();
    }
    let citation = prompt["citations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["origin"]["kind"] == "captured_primary_source")
        .unwrap();
    let mut response = finish(citation["citation_id"].clone());
    let good = response["action"]["findings"][0].clone();
    let mut bad = good.clone();
    bad["title"] = json!("Default MissingStrategy");
    bad["statement"] = json!("MissingStrategy controls this behavior.");
    response["action"]["findings"] = if call == 2 {
        json!([good, bad])
    } else {
        json!([bad, good])
    };
    response
}

#[test]
fn analyst25_partial_survives_advisory_audit_settlement_and_reopen() {
    // Both verdicts publish: the in-run audit remains advisory.
    for verdict in ["pass", "indeterminate"] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(GatedFlow::PartialWithholding);
        scripted.audit_verdict = Some(verdict);
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV25,
            true,
        );
        f.run_with_audit_options(provider.as_ref(), RunClock::new(Instant::now()), None, true);
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
            Some(InvestigationCompletionOutcome::FindingsWithheldAfterCorrection)
        );
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 1);
        assert_eq!(detail.usage.model_invocations, 5);
        assert!(detail.advisory_audit.is_some());
        let result = reopened.investigation_result(&f.id).unwrap().unwrap();
        assert_eq!(result.findings.len(), 1);
        let completion = result.completion.as_ref().unwrap();
        assert_eq!(completion.outcome, agents::investigation::staged_finish::CompletionOutcome::FindingsWithheldAfterCorrection);
        assert_eq!(
            completion.withholding.as_ref().unwrap().withheld_indexes,
            vec![0]
        );
        let payloads = provider.received_payloads.lock().unwrap();
        let audit = payloads
            .iter()
            .find(|payload| {
                serde_json::from_str::<Value>(&payload.prompt).unwrap()["role"]
                    == "candidate-evidence-auditor@3"
            })
            .expect("auditor was dispatched");
        let prompt: Value = serde_json::from_str(&audit.prompt).unwrap();
        assert_eq!(
            prompt["candidate"]["completion_disposition"]["outcome"],
            "findings_withheld_after_correction"
        );
        assert!(!audit.prompt.contains("MissingStrategy"));
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("MissingStrategy")
        );
        drop(payloads);
        if verdict == "pass" {
            assert_partial_parent_child_prompt(&f, &detail);
        }
    }
}

fn assert_partial_parent_child_prompt(f: &NextFixture, parent: &InvestigationDetail) {
    let provider = Arc::new(NextProvider::new(false));
    let mut request = f.request.clone();
    request.request_nonce = "partial-parent-auditor-child".into();
    request.specialist_id = "evidence-auditor@10".into();
    request.parent_id = Some(f.id.clone());
    request.conversation_id = Some(parent.summary.conversation_id.clone());
    let state = f.base.base.app.state::<AppState>();
    let mut jobs = state.jobs.lock().unwrap();
    let descriptor = descriptor(provider.as_ref(), InvestigationProviderMode::Local).unwrap();
    let start = jobs
        .start_next_prompt_investigation(&request, Some(&descriptor))
        .unwrap();
    let id = start.detail.summary.investigation_id;
    let plan = jobs
        .claim_plan(
            start.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let lock = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = jobs.claim_execution(&plan, lock).unwrap();
    drop(jobs);
    let live = state.investigations.insert(&id).unwrap();
    f.base.base.app.run_completion(
        id,
        request,
        execution,
        provider.clone(),
        live,
        Instant::now(),
    );
    let prompts = provider.observed.prompts.lock().unwrap();
    let prompt = prompts
        .first()
        .expect("separate EvidenceAuditor child reached provider boundary");
    let history = prompt["prior_findings_as_t3_history"][0].as_str().unwrap();
    let saved: Value = serde_json::from_str(history).unwrap();
    assert_eq!(saved["parent_investigation_id"], f.id);
    assert_eq!(saved["saved_execution_status"], "completed");
    assert_eq!(
        saved["original_scope"],
        serde_json::to_value(&parent.summary.scope).unwrap()
    );
    assert_eq!(
        saved["saved_result"]["completion"]["outcome"],
        "findings_withheld_after_correction"
    );
    assert!(!history.contains("MissingStrategy"));
}

// AC-0287: exclusion cannot turn an entirely unverified answer into a result.
#[test]
fn analyst25_all_findings_withheld_fails_and_reopens_without_result() {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::AllFindingsWithheld);
    let f = NextFixture::with_source_obligation(&provider, SpecialistId::DomainAnalystV25, true);
    f.run_with_audit_options(&provider, RunClock::new(Instant::now()), None, true);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert!(!detail.summary.has_result);
    assert!(reopened.investigation_result(&f.id).unwrap().is_none());
    assert_ne!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::FindingsWithheldAfterCorrection)
    );
    assert_eq!(detail.usage.model_invocations, 4);
    assert!(detail.advisory_audit.is_none());
}

// AC-0287: withholding cannot waive an independently unmet inventory obligation.
#[test]
fn analyst25_unmet_obligation_prevents_partial_result() {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::PartialWithholding);
    let base = CompletionFixture::new(&provider, SpecialistId::DomainAnalystV9, true, true, false);
    let request = base.request.clone();
    let f =
        NextFixture::from_completion_base(&provider, SpecialistId::DomainAnalystV25, base, request);
    f.run_with_audit_options(&provider, RunClock::new(Instant::now()), None, true);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Failed,
        "{:?}",
        detail.error
    );
    assert!(reopened.investigation_result(&f.id).unwrap().is_none());
    assert!(!detail.summary.has_result);
    assert_ne!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::FindingsWithheldAfterCorrection)
    );
    assert_eq!(detail.usage.model_invocations, 4);
    let prompts = provider.observed.prompts.lock().unwrap();
    let unmet = prompts[3]["completion_metadata"]["feedback"]["unmet"]
        .as_array()
        .unwrap();
    assert!(unmet.iter().any(|item| item["kind"] == "inventory"));
    assert!(!prompts[3]["completion_metadata"]["feedback"]["prose_correction"].is_null());
}
