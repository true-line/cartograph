//! AC-0566: genuine start/query/read/finish boundaries, no saved-state rewriting.
use super::*;
use crate::jobs::investigations::{SemanticAuditResponse, SemanticAuditStart};

#[test]
fn supported_candidate_audit_start_rolls_back_and_reopens_without_publication() {
    run_audit_checkpoint(None);
}

#[test]
fn semantic_audit_settlement_publishes_advisory_and_records_invalid_binding() {
    for verdict in [
        "pass",
        "unsupported_scope",
        "indeterminate",
        "late_pass",
        "forged_binding",
        "invalid_output",
        "transport_failure",
        "length_failure",
    ] {
        run_audit_checkpoint(Some(verdict));
    }
}

#[test]
fn semantic_audit_cancelled_pass_never_publishes() {
    run_audit_checkpoint(Some("cancelled_pass"));
}

#[test]
fn semantic_audit_failed_response_retains_charge_without_publication_or_retry() {
    for failure in ["provider_failure", "invalid_reply"] {
        run_audit_checkpoint(Some(failure));
    }
}

fn run_audit_checkpoint(settlement: Option<&str>) {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(GatedFlow::TypedCorrection);
    let provider = Arc::new(scripted);
    let f = NextFixture::with_source_obligation(
        provider.as_ref(),
        SpecialistId::DomainAnalystV20,
        true,
    );
    let reached = AtomicUsize::new(0);
    let probe = |execution: &JobExecution,
                 detail: &InvestigationDetail,
                 input: &InvestigationInput,
                 original: &PreparedBoundedCompletion|
     -> Result<bool, Failure> {
        let state = f.base.base.app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        let (audit, audit_limits) = match jobs.prepare_next_prompt_semantic_audit(
            execution,
            detail.summary.revision,
            input,
            original,
        ) {
            Ok(audit) => audit,
            Err(InvestigationStoreError::Stale) => return Ok(false), // genuine earlier correction refusal
            Err(error) => panic!("pending candidate replay failed: {error:?}"),
        };
        assert_eq!(reached.fetch_add(1, Ordering::SeqCst), 0);
        assert_eq!(detail.usage.model_invocations, 4);
        assert!(!detail.summary.has_result);
        assert!(jobs.investigation_result(&f.id).unwrap().is_none());
        let path = f.base.base.app_data.join("state.db");
        let reopened = crate::jobs::JobStore::open(&path).unwrap();
        let before = serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap();
        assert_eq!(before, serde_json::to_value(detail).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        let refs: i64 = connection
            .query_row(
                "SELECT count(*) FROM investigation_chat_messages WHERE task_id=?1 AND ordinal=1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(refs, 0);
        let action = audit.action(format!("{}:audit-1", f.id)).unwrap();
        let firewall = firewall(&state).unwrap();
        let prepared = firewall
            .prepare_bounded(provider.as_ref(), &action, &audit_limits)
            .unwrap();
        let authorized = firewall
            .authorize_bounded(provider.as_ref(), &prepared, None)
            .unwrap();
        // Wrong transport limit reaches post-insert validation, so the complete
        // journal creation and debit must roll back in the owning transaction.
        let mut values = audit_limits.values();
        values.request_timeout_ms -= 1;
        let wrong = firewall
            .prepare_bounded(
                provider.as_ref(),
                &action,
                &CompletionLimits::from_audit_values(values).unwrap(),
            )
            .unwrap();
        let wrong_grant = firewall
            .authorize_bounded(provider.as_ref(), &wrong, None)
            .unwrap();
        assert!(
            jobs.begin_next_prompt_semantic_audit(
                execution,
                detail.summary.revision,
                &SemanticAuditStart {
                    separate_auditor: None,
                    input,
                    original_prepared: original,
                    prepared: &wrong,
                    authorized: &wrong_grant,
                    active_milliseconds: detail.usage.active_milliseconds
                },
                |_| Ok(()),
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(jobs.investigation(&f.id).unwrap()).unwrap(),
            before
        );
        let namespaces: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'candidate_audit_*'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(namespaces, 0);
        let started = jobs
            .begin_next_prompt_semantic_audit(
                execution,
                detail.summary.revision,
                &SemanticAuditStart {
                    separate_auditor: None,
                    input,
                    original_prepared: original,
                    prepared: &prepared,
                    authorized: &authorized,
                    active_milliseconds: detail.usage.active_milliseconds,
                },
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            started.usage.model_invocations,
            detail.usage.model_invocations + 1
        );
        assert_eq!(
            started.usage.generated_token_reservations,
            detail.usage.generated_token_reservations + llm::bounded::AUDIT_OUTPUT_TOKENS
        );
        assert!(started.summary.invocation_pending);
        assert!(!started.summary.has_result);
        for revision in [detail.summary.revision, started.summary.revision] {
            assert!(
                jobs.begin_next_prompt_semantic_audit(
                    execution,
                    revision,
                    &SemanticAuditStart {
                        separate_auditor: None,
                        input,
                        original_prepared: original,
                        prepared: &prepared,
                        authorized: &authorized,
                        active_milliseconds: detail.usage.active_milliseconds
                    },
                    |_| panic!("no second dispatch witness"),
                )
                .is_err()
            );
        }
        if let Some(verdict) = settlement {
            if matches!(verdict, "provider_failure" | "invalid_reply") {
                let reason = if verdict == "provider_failure" {
                    InvestigationStopReason::OutcomeUnknown
                } else {
                    InvestigationStopReason::InvalidCompletionEnvelope
                };
                let stopped = jobs
                    .stop_next_prompt_investigation(
                        execution,
                        started.summary.revision,
                        reason,
                        started.limits.active_seconds * 1000 + 1,
                    )
                    .unwrap();
                assert_eq!(
                    stopped.summary.status,
                    if verdict == "provider_failure" {
                        InvestigationStatus::OutcomeUnknown
                    } else {
                        InvestigationStatus::Failed
                    }
                );
                assert_eq!(stopped.usage.model_invocations, 5);
                assert_eq!(
                    stopped.usage.generated_token_reservations,
                    started.usage.generated_token_reservations
                );
                assert_eq!(
                    stopped.usage.active_milliseconds,
                    started.limits.active_seconds * 1000 + 1
                );
                assert_eq!(stopped.usage.reported_output_tokens, None);
                assert!(!stopped.summary.has_result);
                assert!(jobs.investigation_result(&f.id).unwrap().is_none());
                for revision in [started.summary.revision, stopped.summary.revision] {
                    assert!(
                        jobs.stop_next_prompt_investigation(
                            execution,
                            revision,
                            reason,
                            stopped.usage.active_milliseconds
                        )
                        .is_err()
                    );
                }
                drop(jobs);
                let reopened = crate::jobs::JobStore::open(&path).unwrap();
                assert_eq!(
                    serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap(),
                    serde_json::to_value(&stopped).unwrap()
                );
                let receipts: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM candidate_audit_responses WHERE task_id=?1",
                        [&f.id],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(receipts, 0);
                return Ok(true);
            }
            let wire_verdict = if matches!(
                verdict,
                "late_pass"
                    | "cancelled_pass"
                    | "forged_binding"
                    | "invalid_output"
                    | "transport_failure"
                    | "length_failure"
            ) {
                "pass"
            } else {
                verdict
            };
            let started = if verdict == "cancelled_pass" {
                let cancelled = jobs.cancel_next_prompt_investigation(&f.id).unwrap();
                assert_eq!(cancelled.usage, started.usage);
                assert!(cancelled.summary.cancel_requested);
                cancelled
            } else {
                started.clone()
            };
            let prompt: serde_json::Value = serde_json::from_str(&audit.payload().prompt).unwrap();
            let reviews = prompt["subjects"].as_array().unwrap().iter().map(|subject|
                    serde_json::json!({"subject":subject["subject"],"assessment":{"verdict":wire_verdict}})).collect::<Vec<_>>();
            let reply = serde_json::json!({"schema_version":1,"candidate_hash":prompt["candidate_hash"],
                    "acquired_ledger_hash":prompt["acquired_ledger_hash"],"contract_hash":prompt["contract_hash"],"reviews":reviews});
            let completion = BoundedCompletion {
                text: reply.to_string(),
                requested_model: prepared.profile().requested_model.clone(),
                response_model: None,
                provider_request_id: None,
                usage: None,
                request_bytes: 1,
                response_bytes: reply.to_string().len() as u64,
            };
            let mut completion = completion;
            if verdict == "forged_binding" {
                let mut bad = reply.clone();
                bad["candidate_hash"] = serde_json::json!("f".repeat(64));
                completion.text = bad.to_string();
            } else if verdict == "invalid_output" {
                completion.text = "not-json".into();
            }
            let response = SemanticAuditResponse {
                input,
                original_prepared: original,
                audit_prepared: &prepared,
                completion: match verdict {
                    "transport_failure" => {
                        Err(agents::investigation::semantic_audit::AuditFailure::Transport)
                    }
                    "length_failure" => {
                        Err(agents::investigation::semantic_audit::AuditFailure::Length)
                    }
                    _ => Ok(&completion),
                },
                active_milliseconds: if verdict == "late_pass" {
                    started.limits.active_seconds * 1000 + 1
                } else {
                    started.usage.active_milliseconds
                },
            };
            let settled = jobs
                .settle_next_prompt_semantic_audit(execution, started.summary.revision, &response)
                .unwrap_or_else(|e| panic!("settlement {verdict}: {e:?}"));
            let publish = !matches!(verdict, "late_pass" | "cancelled_pass");
            assert_eq!(settled.summary.has_result, publish);
            use agents::investigation::semantic_audit::AdvisoryAudit;
            assert!(
                matches!(
                    &settled.advisory_audit,
                    Some(AdvisoryAudit::NotCompleted { .. })
                ) == matches!(
                    verdict,
                    "forged_binding" | "invalid_output" | "transport_failure" | "length_failure"
                )
            );
            assert_eq!(
                settled.summary.status,
                if publish {
                    InvestigationStatus::Completed
                } else if verdict == "cancelled_pass" {
                    InvestigationStatus::Cancelled
                } else {
                    InvestigationStatus::Failed
                }
            );
            assert_eq!(settled.usage.model_invocations, 5);
            assert!(!settled.summary.invocation_pending);
            assert_eq!(jobs.investigation_result(&f.id).unwrap().is_some(), publish);
            for revision in [started.summary.revision, settled.summary.revision] {
                assert!(
                    jobs.settle_next_prompt_semantic_audit(execution, revision, &response)
                        .is_err()
                );
            }
            drop(jobs);
            let reopened = crate::jobs::JobStore::open(&path).unwrap();
            assert_eq!(
                serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap(),
                serde_json::to_value(&settled).unwrap()
            );
            return Ok(true);
        }
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(&path).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap(),
            serde_json::to_value(&started).unwrap()
        );
        assert!(reopened.investigation_result(&f.id).unwrap().is_none());
        let calls: i64 = connection
            .query_row(
                "SELECT count(*) FROM candidate_audit_invocations WHERE task_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(calls, 1);
        Ok(true) // simulated crash checkpoint before any auditor provider call
    };
    f.run_with_audit_probe(
        provider.as_ref(),
        RunClock::new(Instant::now()),
        Some(&probe),
    );
    let state = f.base.base.app.state::<AppState>();
    let detail = state.jobs.lock().unwrap().investigation(&f.id).unwrap();
    assert_eq!(
        reached.load(Ordering::SeqCst),
        1,
        "status={:?} calls={} error={:?}",
        detail.summary.status,
        detail.usage.model_invocations,
        detail.error
    );
    assert_eq!(provider.observed.prompts.lock().unwrap().len(), 4);
    if settlement.is_some() {
        return;
    }
    let before_recovery = detail.usage.clone();
    let NextFixture { execution, .. } = f;
    drop(execution); // release the genuine execution lock, simulating owner death
    let jobs = state.jobs.lock().unwrap();
    let candidate = jobs
        .investigation_recovery_candidates()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.lock_target().id == detail.summary.job_id)
        .unwrap();
    drop(jobs);
    let lock = state
        .job_execution_locks
        .try_reserve(candidate.lock_target())
        .unwrap();
    let mut jobs = state.jobs.lock().unwrap();
    let recovered = jobs
        .recover_investigation(&candidate, lock)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.summary.status,
        InvestigationStatus::OutcomeUnknown
    );
    assert_eq!(recovered.usage, before_recovery);
    assert!(!recovered.summary.has_result);
    assert!(!recovered.summary.invocation_pending);
    assert!(
        jobs.investigation_result(&detail.summary.investigation_id)
            .unwrap()
            .is_none()
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        serde_json::to_value(
            reopened
                .investigation(&detail.summary.investigation_id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&recovered).unwrap()
    );
    let lock = state
        .job_execution_locks
        .try_reserve(candidate.lock_target())
        .unwrap();
    let mut jobs = state.jobs.lock().unwrap();
    assert!(
        jobs.recover_investigation(&candidate, lock)
            .unwrap()
            .is_none()
    );
    assert_eq!(provider.observed.prompts.lock().unwrap().len(), 4);
}

#[test]
fn semantic_audit_worker_dispatches_once_with_original_deadline_and_reopens() {
    for verdict in ["pass", "unsupported_scope", "invalid"] {
        run_audit_dispatch(verdict);
    }
}

#[test]
fn semantic_audit_worker_failure_and_inflight_cancellation_keep_charge() {
    for verdict in ["provider_failure", "audit_timeout", "cancelled_pass"] {
        run_audit_dispatch(verdict);
    }
}

// AC-0566: invalid audit output is recorded only as not-completed, never retained as reviews.
#[test]
fn semantic_audit_worker_duplicate_subject_and_foreign_citation_record_not_completed() {
    for verdict in ["duplicate_subject", "foreign_citation"] {
        run_audit_dispatch(verdict);
    }
}

fn run_audit_dispatch(verdict: &'static str) {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::TypedCorrection);
    provider.audit_verdict = Some(verdict);
    let f = NextFixture::with_source_obligation(&provider, SpecialistId::DomainAnalystV20, true);
    if verdict == "cancelled_pass" {
        let app = f.base.base.app.handle().clone();
        let id = f.id.clone();
        *provider.audit_cancel.lock().unwrap() = Some(Box::new(move || {
            let state = app.state::<AppState>();
            let mut jobs = state.jobs.lock().unwrap();
            let before = jobs.investigation(&id).unwrap();
            assert_eq!(before.usage.model_invocations, 5);
            let cancelled = jobs.cancel_next_prompt_investigation(&id).unwrap();
            assert!(cancelled.summary.cancel_requested);
            assert_eq!(cancelled.usage, before.usage);
        }));
    }
    f.run_with_audit_options(&provider, RunClock::new(Instant::now()), None, true);
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 5);
    assert_eq!(detail.usage.model_invocations, 5);
    assert!(
        terminal(detail.summary.status),
        "{verdict}: {:?}",
        detail.summary.status
    );
    let publish = verdict != "cancelled_pass";
    assert_eq!(detail.summary.has_result, publish, "{verdict}");
    assert_eq!(jobs.investigation_result(&f.id).unwrap().is_some(), publish);
    if matches!(
        verdict,
        "provider_failure" | "audit_timeout" | "cancelled_pass"
    ) {
        assert_eq!(
            detail.summary.status,
            if verdict == "cancelled_pass" {
                InvestigationStatus::Cancelled
            } else {
                InvestigationStatus::Completed
            }
        );
        assert_eq!(
            detail.usage.generated_token_reservations,
            4 * detail.limits.generated_tokens_per_invocation + llm::bounded::AUDIT_OUTPUT_TOKENS
        );
        let db = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let receipts: i64 = db
            .query_row("SELECT count(*) FROM candidate_audit_responses", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(receipts, 1);
    }
    if matches!(
        verdict,
        "invalid" | "duplicate_subject" | "foreign_citation"
    ) {
        assert_eq!(detail.summary.status, InvestigationStatus::Completed);
        assert_eq!(
            detail.usage.generated_token_reservations,
            4 * detail.limits.generated_tokens_per_invocation + llm::bounded::AUDIT_OUTPUT_TOKENS
        );
        let db = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let receipts: i64 = db
            .query_row("SELECT count(*) FROM candidate_audit_responses", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(receipts, 1);
        assert!(matches!(
            detail.advisory_audit,
            Some(agents::investigation::semantic_audit::AdvisoryAudit::NotCompleted { .. })
        ));
        assert!(
            !serde_json::to_string(&detail)
                .unwrap()
                .contains("foreign-audit-witness-canary")
        );
    }
    let deadlines = provider.deadlines.lock().unwrap();
    assert_eq!(deadlines.len(), 5);
    // RunClock rounds active samples to milliseconds; never a fresh audit allowance.
    assert!(
        deadlines[4]
            .duration_since(deadlines[0])
            .max(deadlines[0].duration_since(deadlines[4]))
            < Duration::from_millis(10)
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        serde_json::to_value(&detail).unwrap(),
        serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap()
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 5);
}

// AC-0566: the explicit development switch crosses the real completion entry.
#[test]
fn semantic_audit_development_switch_uses_real_completion_entry() {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::TypedCorrection);
    provider.audit_verdict = Some("pass");
    let provider = Arc::new(provider);
    let f = NextFixture::with_source_obligation(
        provider.as_ref(),
        SpecialistId::DomainAnalystV20,
        true,
    );
    f.base.base.app.run_completion_with_audit(
        f.id.clone(),
        f.request.clone(),
        f.execution.clone(),
        provider.clone(),
        f.live.clone(),
        Instant::now(),
        true,
        None,
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 5);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert!(detail.summary.has_result);
    assert_eq!(detail.usage.model_invocations, 5);
}

// AC-0566: expiration after the durable boundary must not roll terminal state back.
#[test]
fn semantic_audit_analyst_boundary_timeout_keeps_charge_and_reopens() {
    let mut provider = NextProvider::new(false);
    provider.gated_flow = Some(GatedFlow::TypedCorrection);
    provider.audit_verdict = Some("pass");
    let f = NextFixture::with_source_obligation(&provider, SpecialistId::DomainAnalystV20, true);
    f.run_with_audit_options(&provider, RunClock::expiring_at_invocation(2), None, true);
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(detail.usage.model_invocations, 2);
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert!(
        detail
            .error
            .as_deref()
            .unwrap()
            .contains("deadline_exceeded")
    );
    assert!(!detail.summary.has_result);
    assert!(!detail.summary.invocation_pending);
    assert_eq!(
        detail.usage.generated_token_reservations,
        2 * detail.limits.generated_tokens_per_invocation
    );
    assert_eq!(detail.usage.reported_input_tokens, None);
    assert_eq!(detail.usage.reported_output_tokens, None);
    assert!(detail.usage.active_milliseconds > detail.limits.active_seconds * 1000);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        serde_json::to_value(&detail).unwrap(),
        serde_json::to_value(reopened.investigation(&f.id).unwrap()).unwrap()
    );
}

// AC-0566: distinct configured provider is used only for the audit call.
struct SeparateTestAuditor {
    same_model: bool,
    inner: Arc<NextProvider>,
    calls: AtomicUsize,
}
impl LlmProvider for SeparateTestAuditor {
    fn id(&self) -> &str {
        "local:separate-auditor"
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        self.inner.capabilities()
    }
    fn embed(&self, text: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.inner.embed(text)
    }
    fn supports_bounded_response_schema(&self) -> bool {
        true
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        let mut profile = self.inner.bounded_profile()?;
        profile.provider_id = self.id().into();
        if !self.same_model {
            profile.requested_model = "separate-audit-model".into();
        }
        profile.endpoint_id = "http://127.0.0.1:9999".into();
        Ok(profile)
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        assert_eq!(prompt["role"], "candidate-evidence-auditor@3");
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut reply = self.inner.complete_bounded(request, control)?;
        reply.requested_model = "separate-audit-model".into();
        reply.response_model = Some("separate-audit-model-observed".into());
        Ok(reply)
    }
}

#[test]
fn semantic_audit_separate_provider_identity_and_failures_reopen() {
    for verdict in ["pass", "provider_failure", "audit_timeout"] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(GatedFlow::TypedCorrection);
        scripted.audit_verdict = Some(verdict);
        let analyst = Arc::new(scripted);
        let auditor = Arc::new(SeparateTestAuditor {
            inner: analyst.clone(),
            same_model: false,
            calls: AtomicUsize::new(0),
        });
        let f = NextFixture::with_source_obligation(
            analyst.as_ref(),
            SpecialistId::DomainAnalystV20,
            true,
        );
        f.base.base.app.run_completion_with_audit(
            f.id.clone(),
            f.request.clone(),
            f.execution.clone(),
            analyst.clone(),
            f.live.clone(),
            Instant::now(),
            true,
            Some(ConfiguredAuditor {
                provider: auditor.clone(),
                reported_model_digest: Some("a".repeat(64)),
                analyst_reported_model_digest: Some("b".repeat(64)),
            }),
        );
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        let detail = reopened.investigation(&f.id).unwrap();
        assert_eq!(auditor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(detail.usage.model_invocations, 5);
        assert!(detail.summary.has_result, "{:?}", detail.error);
        assert!(!detail.summary.invocation_pending);
        assert!(terminal(detail.summary.status));
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let body: Vec<u8> = connection
            .query_row(
                "SELECT body FROM candidate_audit_invocations WHERE task_id=?1",
                [&f.id],
                |r| r.get(0),
            )
            .unwrap();
        let invocation: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            invocation["provider"]["requested_model"],
            "separate-audit-model"
        );
        assert_eq!(
            invocation["separate_auditor"]["reported_model_digest"],
            "a".repeat(64)
        );
        let state = f.base.base.app.state::<AppState>();
        assert_eq!(
            state.jobs.lock().unwrap().investigation(&f.id).unwrap(),
            detail
        );
    }
}

#[test]
fn semantic_audit_separate_provider_refuses_identical_descriptor_without_debit() {
    let mut scripted = NextProvider::new(false);
    scripted.gated_flow = Some(GatedFlow::TypedCorrection);
    scripted.audit_verdict = Some("pass");
    let analyst = Arc::new(scripted);
    let f =
        NextFixture::with_source_obligation(analyst.as_ref(), SpecialistId::DomainAnalystV20, true);
    f.base.base.app.run_completion_with_audit(
        f.id.clone(),
        f.request.clone(),
        f.execution.clone(),
        analyst.clone(),
        f.live.clone(),
        Instant::now(),
        true,
        Some(ConfiguredAuditor {
            provider: analyst.clone(),
            reported_model_digest: None,
            analyst_reported_model_digest: None,
        }),
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    let detail = reopened.investigation(&f.id).unwrap();
    assert_eq!(analyst.observed.calls.load(Ordering::SeqCst), 4);
    assert_eq!(detail.usage.model_invocations, 4);
    assert!(!detail.summary.has_result);
    assert!(!detail.summary.invocation_pending);
    assert!(terminal(detail.summary.status));
}

#[test]
fn semantic_audit_separate_provider_refuses_same_model_other_endpoint_or_equal_digest_without_debit()
 {
    for same_model in [true, false] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(GatedFlow::TypedCorrection);
        scripted.audit_verdict = Some("pass");
        let analyst = Arc::new(scripted);
        let auditor = Arc::new(SeparateTestAuditor {
            inner: analyst.clone(),
            calls: AtomicUsize::new(0),
            same_model,
        });
        let analyst_profile = analyst.bounded_profile().unwrap();
        let auditor_profile = auditor.bounded_profile().unwrap();
        assert_ne!(analyst_profile.endpoint_id, auditor_profile.endpoint_id);
        assert_eq!(
            analyst_profile.requested_model == auditor_profile.requested_model,
            same_model
        );
        let f = NextFixture::with_source_obligation(
            analyst.as_ref(),
            SpecialistId::DomainAnalystV20,
            true,
        );
        f.base.base.app.run_completion_with_audit(
            f.id.clone(),
            f.request.clone(),
            f.execution.clone(),
            analyst.clone(),
            f.live.clone(),
            Instant::now(),
            true,
            Some(ConfiguredAuditor {
                provider: auditor.clone(),
                reported_model_digest: Some("a".repeat(64)),
                analyst_reported_model_digest: Some(if same_model { "b" } else { "a" }.repeat(64)),
            }),
        );
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        let detail = reopened.investigation(&f.id).unwrap();
        assert_eq!(analyst.observed.calls.load(Ordering::SeqCst), 4);
        assert_eq!(detail.usage.model_invocations, 4);
        assert!(!detail.summary.has_result);
        assert!(!detail.summary.invocation_pending);
        assert!(terminal(detail.summary.status));
        assert_eq!(auditor.calls.load(Ordering::SeqCst), 0);
    }
}
