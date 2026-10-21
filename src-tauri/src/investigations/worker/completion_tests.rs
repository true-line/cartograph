use super::*;
use agents::investigation::staged_attempt::AttemptOutcome;

fn export_wire(name: &str, detail: &InvestigationDetail, result: Option<&InvestigationResult>) {
    if let Some(directory) = std::env::var_os("CARTOGRAPH_COMPLETION_UI_FIXTURE_DIR") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("completion-v7-{name}.json")),
            serde_json::to_vec_pretty(&json!({"detail":detail,"result":result})).unwrap(),
        )
        .unwrap();
    }
}

pub(super) fn scripted_action(mode: Mode, call: usize, prompt: &Value) -> Value {
    let progressive = matches!(
        mode,
        Mode::ProgressiveReplay
            | Mode::ProgressiveReplayWrongContext
            | Mode::ProgressiveReplayWrongIndex
            | Mode::ProgressiveReplayWrongId
    );
    if progressive && call == 1 {
        return json!({"type":"query_context","context_id":prompt["context_id"],"query":{"scope":{"type":"all"},"kind":"edge","labels":["NoSuchFixtureLabel"],"max_facts":1,"max_bytes":32768,"cursor":null}});
    }
    if progressive && call == 2 {
        let reference = &prompt["compact_context"]["query_history"]["prior_queries"][0];
        let mut replay = json!({"type":"replay_query","context_id":prompt["context_id"],"query_index":reference["query_index"],"query_id":reference["query_id"]});
        match mode {
            Mode::ProgressiveReplayWrongContext => {
                replay["context_id"] = json!(format!("context-v2:{}", "0".repeat(64)))
            }
            Mode::ProgressiveReplayWrongIndex => replay["query_index"] = json!(999),
            Mode::ProgressiveReplayWrongId => replay["query_id"] = json!("0".repeat(64)),
            _ => (),
        }
        return replay;
    }
    let call = if progressive {
        call.saturating_sub(2)
    } else {
        call
    };
    let obligations = prompt["completion_contract"]["obligations"]["obligations"]
        .as_array()
        .unwrap();
    let source = obligations.iter().find(|item| item["kind"] == "source");
    let inventory = obligations.iter().find(|item| item["kind"] == "inventory");
    let insufficient = matches!(mode, Mode::CompletionNoAttempt)
        || (matches!(mode, Mode::CompletionInventoryUnavailable) && call > 0)
        || (matches!(mode, Mode::CompletionRedacted) && call == 3);
    if insufficient {
        return json!({"schema_version":1,"outcome":"insufficient_evidence","action":{"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence","limitations":["The completed bounded read cannot establish source behavior."]}});
    }
    if matches!(mode, Mode::CompletionInventoryUnavailable) {
        let target = &inventory.unwrap()["target"];
        return json!({"type":"read_coverage_inventory","context_id":target["context_id"],"repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":1});
    }
    if call == 0 {
        return json!({"type":"query_context","context_id":prompt["context_id"],"query":{"scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],"max_facts":1,"max_bytes":32768,"cursor":null}});
    }
    let read_call = if matches!(mode, Mode::CompletionRepair) {
        2
    } else {
        1
    };
    if let Some(source) = source.filter(|_| {
        call == read_call && !matches!(mode, Mode::CompletionCap | Mode::CompletionCancelRefused)
    }) {
        let target = &source["target"];
        return json!({"type":"read_evidence","fact":target["fact"],"role":target["role"],"index":target["index"]});
    }
    if let Some(inventory) = inventory.filter(|_| {
        call == read_call + usize::from(source.is_some())
            && !matches!(mode, Mode::CompletionCap | Mode::CompletionCancelRefused)
    }) {
        let target = &inventory["target"];
        return json!({"type":"read_coverage_inventory","context_id":target["context_id"],"repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768});
    }
    let citations = prompt["citations"].as_array().unwrap();
    let citation = citations
        .iter()
        .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
        .unwrap_or(&citations[0]);
    json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"A bounded guarded path","statement":"The scoped observation suggests a conditional exit; broader policy remains unresolved.","citation_ids":[citation["citation_id"]],"limitations":["Runtime inputs remain unknown."]}],"knowledge_completeness":"partial","limitations":["This scoped result establishes no complete business policy."]}})
}

struct CompletionFixture {
    base: Fixture,
    request: StagedObligationRequest,
    id: String,
    execution: JobExecution,
    live: Arc<LiveTask>,
}
impl CompletionFixture {
    fn new(
        provider: &dyn LlmProvider,
        specialist: SpecialistId,
        source: bool,
        inventory: bool,
        redacted: bool,
    ) -> Self {
        // Reuse real parser/receipt/context setup, then retire its unused legacy
        // task before creating the genuine request4 task under test.
        let base = Fixture::with_contract(
            provider,
            tempfile::tempdir().unwrap(),
            "Inspect selected source and recorded analysis limits.",
            SpecialistId::DomainAnalystV6,
            source.then_some(agents::TaskRangeRole::DefinitionInitializer),
            if redacted {
                "export function enabled(ok: boolean) { const limited = ok /* password=syntheticsecret123 */ === false; if (limited) return false; }\n"
            } else {
                ORIGINAL
            },
        );
        let state = base.app.state::<AppState>();
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.cancel_investigation(&base.id).unwrap();
            let revision = jobs.investigation(&base.id).unwrap().summary.revision;
            jobs.advance_investigation(
                &base.execution,
                revision,
                InvestigationTransition::StopWithElapsed {
                    reason: InvestigationStopReason::Cancelled,
                    active_milliseconds: 0,
                },
            )
            .unwrap();
        }
        state.investigations.remove(&base.id);
        let capacity = state
            .jobs
            .lock()
            .unwrap()
            .investigation_coverage_archive_capacity(&base.id)
            .unwrap();
        let context = FrozenContext::prepare_with_app(
            base.app.handle(),
            &state,
            &base.request,
            &[],
            Some(capacity),
        )
        .unwrap();
        let proof = context.ledger.coverage.as_ref().unwrap();
        let mut items = Vec::new();
        if source {
            items.push(Obligation::Source {
                obligation_id: String::new(),
                target: Box::new(base.request.inspection_requirements.as_ref().unwrap()[0].clone()),
            });
        }
        if inventory {
            let repo = proof
                .qualification
                .repos
                .iter()
                .find(|repo| repo.inventory_hash.is_some())
                .unwrap();
            items.push(Obligation::Inventory {
                obligation_id: String::new(),
                target: InventoryTarget {
                    context_id: proof.context_id.clone(),
                    repo: repo.repo.clone(),
                    inventory_hash: repo.inventory_hash.clone().unwrap(),
                },
            });
        }
        for item in &mut items {
            let id = item.content_id().unwrap();
            match item {
                Obligation::Source { obligation_id, .. }
                | Obligation::Inventory { obligation_id, .. } => *obligation_id = id,
            }
        }
        let policy = if specialist.uses_policy_selection() {
            &INVESTIGATION_V2_POLICY
        } else {
            &INVESTIGATION_V1_POLICY
        };
        let request = StagedObligationRequest {
            schema_version: if specialist.uses_query_history_disclosure() {
                6
            } else if specialist.uses_policy_selection() {
                5
            } else {
                4
            },
            request_nonce: "completion-worker-fixture".into(),
            specialist_id: serde_json::to_value(specialist)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            obligations: ObligationEnvelope {
                schema_version: if specialist.uses_policy_selection() {
                    2
                } else {
                    1
                },
                contract: if specialist.uses_policy_selection() {
                    "investigation-completion-obligations@2"
                } else {
                    "investigation-completion-obligations@1"
                }
                .into(),
                obligations: items,
            }
            .normalized_with_policy(policy)
            .unwrap(),
            inspection_requirements: None,
            question: base.request.question.clone(),
            scope: base.request.scope.clone(),
            provider_mode: base.request.provider_mode,
            limit_profile: policy.profile().into(),
            expected_graph_revision: base.request.expected_graph_revision.clone(),
            expected_context_id: proof.context_id.clone(),
            conversation_id: None,
            parent_id: None,
        };
        let mut jobs = state.jobs.lock().unwrap();
        let start = jobs
            .start_completion_investigation(
                &request,
                Some(&descriptor(provider, InvestigationProviderMode::Local).unwrap()),
            )
            .unwrap();
        let id = start.detail.summary.investigation_id;
        let plan = jobs
            .claim_plan(
                start.detail.summary.job_id,
                crate::jobs::ClaimMode::StartQueued,
            )
            .unwrap();
        let reservation = state
            .job_execution_locks
            .try_reserve(plan.lock_target())
            .unwrap();
        let (_, execution) = jobs.claim_execution(&plan, reservation).unwrap();
        drop(jobs);
        let live = state.investigations.insert(&id).unwrap();
        Self {
            base,
            request,
            id,
            execution,
            live,
        }
    }
    fn run(&self, provider: Arc<dyn LlmProvider>) {
        run_completion(
            self.base.app.handle().clone(),
            self.id.clone(),
            self.request.clone(),
            self.execution.clone(),
            provider,
            self.live.clone(),
            Instant::now(),
        );
    }
}

#[path = "progressive_tests.rs"]
mod progressive_tests;

#[path = "next_prompt_tests.rs"]
mod next_prompt_tests;

// AC-0266: actual worker chooses each action; one refusal returns protected
// feedback, then ordinary source/inventory reads support a real result5.
#[test]
fn completion_worker_mixed_supported_and_repair_reopen_exactly() {
    for (mode, specialist, calls, refusals) in [
        (
            Mode::CompletionSupported,
            SpecialistId::DomainAnalystV7,
            4,
            0,
        ),
        (
            Mode::CompletionRepair,
            SpecialistId::EvidenceAuditorV7,
            5,
            1,
        ),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(provider.as_ref(), specialist, true, true, false);
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "error={:?}, usage={:?}",
            detail.error,
            detail.usage
        );
        assert_eq!(detail.summary.schema_version, 4);
        assert_eq!(
            detail.summary.completion_outcome,
            Some(InvestigationCompletionOutcome::Supported)
        );
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, refusals);
        assert_eq!(detail.usage.tool_actions, 3);
        assert_eq!(provider.calls.load(Ordering::SeqCst), calls);
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.schema_version, 5);
        export_wire("supported", &detail, Some(&result));
        assert!(
            result
                .completion
                .as_ref()
                .unwrap()
                .assessment
                .iter()
                .all(|item| item.exactly_supplied)
        );
        assert_eq!(
            jobs.investigation_completion_attempts(&fixture.id)
                .unwrap()
                .len(),
            2
        );
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result.clone())
        );
        if let Some(path) = std::env::var_os("CARTOGRAPH_COMPLETION_UI_FIXTURE") {
            std::fs::write(
                path,
                serde_json::to_vec_pretty(&json!({"detail":detail,"result":result})).unwrap(),
            )
            .unwrap();
        }
    }
}

// AC-0266: two real unmet finishes commit a host-only terminal without a third
// invocation, findings or fabricated model completeness, and survive reopen.
#[test]
fn completion_worker_second_refusal_is_atomic_no_result_terminal() {
    let provider = Arc::new(Scripted::new(Mode::CompletionCap));
    let fixture = CompletionFixture::new(
        provider.as_ref(),
        SpecialistId::DomainAnalystV7,
        true,
        false,
        false,
    );
    fixture.run(provider.clone());
    let state = fixture.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Failed,
        "error={:?}, usage={:?}",
        detail.error,
        detail.usage
    );
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::ObligationsUnmet)
    );
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(detail.usage.reported_output_tokens, Some(120));
    assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
    export_wire("terminal", &detail, None);
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    assert!(
        reopened
            .investigation_result(&fixture.id)
            .unwrap()
            .is_none()
    );
}

// AC-0266: an actual frozen inventory read exceeding caller max_bytes records
// unavailable without an acquisition revision and can finish insufficiently.
#[test]
fn completion_worker_inventory_unavailable_is_completed_attempt_not_acquisition() {
    let provider = Arc::new(Scripted::new(Mode::CompletionInventoryUnavailable));
    let fixture = CompletionFixture::new(
        provider.as_ref(),
        SpecialistId::EvidenceAuditorV7,
        false,
        true,
        false,
    );
    fixture.run(provider.clone());
    let state = fixture.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "error={:?}, usage={:?}",
        detail.error,
        detail.usage
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        jobs.investigation_ledger(&fixture.id)
            .unwrap()
            .unwrap()
            .revision,
        0
    );
    let attempts = jobs.investigation_completion_attempts(&fixture.id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert!(matches!(
        attempts[0].outcome,
        AttemptOutcome::Unavailable { .. }
    ));
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    export_wire("insufficient", &detail, Some(&result));
    assert_eq!(
        result.completion.as_ref().unwrap().outcome,
        staged_finish::CompletionOutcome::InsufficientEvidence
    );
    assert_eq!(
        result.completion.as_ref().unwrap().assessment[0].last_attempt_outcome,
        Some(CompletionAttemptOutcome::Unavailable)
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result)
    );
}

// AC-0266: empty insufficient evidence cannot bypass a never-attempted target;
// acquired redacted source stays acquired, rather than a fabricated firewall read.
#[test]
fn completion_worker_insufficient_requires_read_and_redaction_stays_acquired() {
    for (mode, redacted, expected_calls) in [
        (Mode::CompletionNoAttempt, false, 2),
        (Mode::CompletionRedacted, true, 4),
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(
            provider.as_ref(),
            SpecialistId::DomainAnalystV7,
            true,
            false,
            redacted,
        );
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            expected_calls,
            "error={:?}, usage={:?}",
            detail.error,
            detail.usage
        );
        let attempts = jobs.investigation_completion_attempts(&fixture.id).unwrap();
        if redacted {
            assert_eq!(
                detail.summary.status,
                InvestigationStatus::Completed,
                "error={:?}, usage={:?}",
                detail.error,
                detail.usage
            );
            assert_eq!(attempts.len(), 1);
            assert!(matches!(
                attempts[0].outcome,
                AttemptOutcome::SourceAcquired { .. }
            ));
            let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
            let completion = result.completion.as_ref().unwrap();
            assert_eq!(
                completion.outcome,
                staged_finish::CompletionOutcome::InsufficientEvidence
            );
            assert!(
                completion.assessment[0].acquired && !completion.assessment[0].exactly_supplied
            );
            assert_eq!(
                completion.assessment[0].last_attempt_outcome,
                Some(CompletionAttemptOutcome::Acquired)
            );
        } else {
            assert_eq!(
                detail.summary.status,
                InvestigationStatus::Failed,
                "error={:?}, usage={:?}",
                detail.error,
                detail.usage
            );
            assert!(attempts.is_empty());
            assert!(jobs.investigation_result(&fixture.id).unwrap().is_none());
        }
    }
}

// AC-0266: homogeneous envelopes still use the same genuine V7 admission;
// inventory detail is never promoted to a graph/source citation.
#[test]
fn completion_worker_source_only_and_inventory_only_supported() {
    for (source, inventory) in [(true, false), (false, true)] {
        let provider = Arc::new(Scripted::new(Mode::CompletionSupported));
        let fixture = CompletionFixture::new(
            provider.as_ref(),
            SpecialistId::DomainAnalystV7,
            source,
            inventory,
            false,
        );
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "error={:?}, usage={:?}",
            detail.error,
            detail.usage
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.completion.as_ref().unwrap().assessment.len(), 1);
        if inventory {
            assert!(
                result
                    .findings
                    .iter()
                    .flat_map(|finding| &finding.citation_ids)
                    .all(|id| id.starts_with("fact-"))
            );
        }
    }
}

// AC-0266: a late accepted finish preserves existing cancellation semantics;
// an unmet late finish cannot schedule repair, and unknown ownership stays real.
#[test]
fn completion_worker_cancellation_and_unknown_do_not_authorize_repair() {
    for mode in [
        Mode::CancelFinish,
        Mode::CompletionCancelRefused,
        Mode::Unknown,
    ] {
        let provider = Arc::new(Scripted::new(mode));
        let fixture = CompletionFixture::new(
            provider.as_ref(),
            SpecialistId::DomainAnalystV7,
            true,
            false,
            false,
        );
        if !matches!(mode, Mode::Unknown) {
            let app = fixture.base.app.handle().clone();
            let id = fixture.id.clone();
            *provider.cancel.lock().unwrap() = Some(Box::new(move || {
                app.state::<AppState>()
                    .jobs
                    .lock()
                    .unwrap()
                    .cancel_investigation(&id)
                    .unwrap();
            }));
        }
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        let result = jobs.investigation_result(&fixture.id).unwrap();
        match mode {
            Mode::CancelFinish => {
                assert_eq!(
                    detail.summary.status,
                    InvestigationStatus::Completed,
                    "error={:?}, usage={:?}",
                    detail.error,
                    detail.usage
                );
                assert!(detail.summary.cancel_requested && result.is_some());
                assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
            }
            Mode::CompletionCancelRefused => {
                assert_eq!(
                    detail.summary.status,
                    InvestigationStatus::Cancelled,
                    "error={:?}, usage={:?}",
                    detail.error,
                    detail.usage
                );
                assert!(result.is_none());
                assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
                assert!(detail.summary.completion_outcome.is_none());
            }
            Mode::Unknown => {
                assert_eq!(
                    detail.summary.status,
                    InvestigationStatus::OutcomeUnknown,
                    "error={:?}, usage={:?}",
                    detail.error,
                    detail.usage
                );
                assert!(result.is_none());
                assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
                assert!(detail.summary.completion_outcome.is_none());
            }
            _ => unreachable!(),
        }
    }
}

// AC-0266: cap exhaustion remains a limit error before unavailable classification;
// guarded range absence never fabricates acquisition or changes the ledger.
#[test]
fn completion_source_unavailable_preserves_caps_and_operational_errors() {
    struct Sink {
        usage: InvestigationUsage,
    }
    impl AcquisitionSink for Sink {
        fn usage(&self) -> &InvestigationUsage {
            &self.usage
        }
        fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError> {
            self.usage.captured_validation_bytes += bytes;
            Ok(())
        }
        fn publish_input(
            &mut self,
            _: &InvestigationInputLedger,
            usage: InvestigationUsage,
        ) -> Result<(), HostError> {
            self.usage = usage;
            Ok(())
        }
    }
    let provider = Scripted::new(Mode::CompletionSupported);
    let fixture =
        CompletionFixture::new(&provider, SpecialistId::DomainAnalystV7, true, false, false);
    let state = fixture.base.app.state::<AppState>();
    let projected = FrozenContext::completion_acquisition_request(&fixture.request).unwrap();
    let capacity = state
        .jobs
        .lock()
        .unwrap()
        .investigation_coverage_archive_capacity(&fixture.id)
        .unwrap();
    let mut context = FrozenContext::prepare_with_app(
        fixture.base.app.handle(),
        &state,
        &projected,
        &[],
        Some(capacity),
    )
    .unwrap();
    let mut sink = Sink {
        usage: InvestigationUsage::default(),
    };
    context
        .query(
            &state,
            InvestigationQuery {
                scope: InvestigationScope::All,
                kind: Some(InvestigationFactKind::Node),
                labels: vec!["BusinessRule".into()],
                max_facts: 1,
                max_bytes: 32768,
                cursor: None,
            },
            &mut sink,
        )
        .unwrap();
    let target = fixture.request.obligations.sources().unwrap().remove(0);
    let original = context.ledger.clone();
    assert_eq!(
        context.read_evidence_completion(&state, target.fact.clone(), target.role, 1024, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(
        context.read_evidence_completion(&state, target.fact.clone(), target.role, 999, &mut sink),
        Ok(Some(staged_attempt::UnavailableReason::RangeOutOfBounds))
    );
    assert_eq!(context.ledger, original);
    context.evidence = (0..12)
        .map(|index| agents::AgentEvidence {
            id: format!("cap-{index}"),
            source: target.source.clone(),
            text: "retained".into(),
        })
        .collect();
    assert_eq!(
        context.read_evidence_completion(&state, target.fact.clone(), target.role, 999, &mut sink),
        Err(HostError::LimitExceeded)
    );
    context.evidence.clear();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = state.sources.lock().unwrap();
        panic!("simulate operational source registry failure");
    }));
    assert_eq!(
        context.read_evidence_completion(&state, target.fact, target.role, target.index, &mut sink),
        Err(HostError::Operational)
    );
}

// AC-0188: completion acquisition preserves its admitted family and selected
// policy without projecting through the legacy request's 8 KiB / 12-item caps.
#[test]
fn completion_v2_acquisition_preserves_role_and_policy_above_legacy_caps() {
    let provider = Scripted::new(Mode::CompletionSupported);
    let fixture =
        CompletionFixture::new(&provider, SpecialistId::DomainAnalystV7, true, false, false);
    let mut request = fixture.request.clone();
    request.schema_version = 5;
    request.specialist_id = "domain-analyst@8".into();
    request.limit_profile = "investigation-v2".into();
    request.obligations.schema_version = 2;
    request.obligations.contract = "investigation-completion-obligations@2".into();
    request.obligations.obligations = vec![request.obligations.obligations[0].clone(); 13];
    assert!(serde_json::to_vec(&request).unwrap().len() > 8192);
    let registered = FrozenContext::completion_acquisition_request(&request).unwrap();
    assert_eq!(registered.specialist_id, SpecialistId::DomainAnalystV8);
    assert_eq!(
        registered.policy.profile(),
        INVESTIGATION_V2_POLICY.profile()
    );
    let acquisition = FrozenContext::completion_acquisition_request_with_policy(
        &request,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert_eq!(acquisition.specialist_id, SpecialistId::DomainAnalystV8);
    let mut auditor_request = request.clone();
    auditor_request.specialist_id = "evidence-auditor@8".into();
    let auditor = FrozenContext::completion_acquisition_request_with_policy(
        &auditor_request,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap();
    assert_eq!(auditor.specialist_id, SpecialistId::EvidenceAuditorV8);
    assert_eq!(acquisition.policy.profile(), "investigation-v2");
    assert_eq!(
        acquisition.inspection_requirements.as_ref().unwrap().len(),
        1
    );
    let state = fixture.base.app.state::<AppState>();
    let mut context = FrozenContext::prepare_with_app(
        fixture.base.app.handle(),
        &state,
        &acquisition,
        &[],
        Some(16 * 1024 * 1024),
    )
    .unwrap();
    request
        .validate_ledger_with_policy(&context.ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    struct Sink(InvestigationUsage);
    impl AcquisitionSink for Sink {
        fn usage(&self) -> &InvestigationUsage {
            &self.0
        }
        fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError> {
            self.0.captured_validation_bytes += bytes;
            Ok(())
        }
        fn publish_input(
            &mut self,
            _: &InvestigationInputLedger,
            usage: InvestigationUsage,
        ) -> Result<(), HostError> {
            self.0 = usage;
            Ok(())
        }
    }
    let mut sink = Sink(InvestigationUsage::default());
    context
        .query(
            &state,
            InvestigationQuery {
                scope: InvestigationScope::All,
                kind: Some(InvestigationFactKind::Node),
                labels: vec!["BusinessRule".into()],
                max_facts: 1,
                max_bytes: 32768,
                cursor: None,
            },
            &mut sink,
        )
        .unwrap();
    let target = &acquisition.inspection_requirements.as_ref().unwrap()[0];
    for _ in 0..13 {
        context
            .read_evidence_completion(
                &state,
                target.fact.clone(),
                target.role,
                target.index,
                &mut sink,
            )
            .unwrap();
    }
    assert_eq!(context.evidence.len(), 13);
    assert!(context.ledger.validate().is_err());
    context
        .ledger
        .validate_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let length = context.evidence[0].text.len();
    sink.0.evidence_bytes = 64 * 1024 - length;
    context
        .read_evidence_completion(
            &state,
            target.fact.clone(),
            target.role,
            target.index,
            &mut sink,
        )
        .unwrap();
    assert_eq!(sink.0.evidence_bytes, 64 * 1024);
    assert_eq!(
        context.read_evidence_completion(
            &state,
            target.fact.clone(),
            target.role,
            target.index,
            &mut sink
        ),
        Err(HostError::LimitExceeded)
    );
    sink.0.evidence_bytes = context.evidence.iter().map(|item| item.text.len()).sum();
    while context.evidence.len() < 56 {
        context
            .read_evidence_completion(
                &state,
                target.fact.clone(),
                target.role,
                target.index,
                &mut sink,
            )
            .unwrap();
    }
    assert_eq!(context.evidence.len(), 56);
    assert_eq!(
        context.read_evidence_completion(
            &state,
            target.fact.clone(),
            target.role,
            target.index,
            &mut sink
        ),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

// AC-0281: the real worker uses the selected immutable policy at transport,
// preparation, exact finish admission and retained-result replay.
#[test]
fn completion_v2_worker_accepts_selected_policy_and_reopens_exactly() {
    struct PolicyCheckedProvider(Scripted);
    impl LlmProvider for PolicyCheckedProvider {
        fn id(&self) -> &str {
            self.0.id()
        }
        fn locality(&self) -> Locality {
            self.0.locality()
        }
        fn capabilities(&self) -> ProviderCaps {
            self.0.capabilities()
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            self.0.embed(texts)
        }
        fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
            self.0.bounded_profile()
        }
        fn complete_bounded(
            &self,
            request: &AuthorizedBoundedCompletion,
            control: &CompletionControl<'_>,
        ) -> Result<BoundedCompletion, BoundedCallError> {
            let limits = request.limits().values();
            assert_eq!(limits.max_spans, 56);
            assert_eq!(limits.input_bytes, 128 * 1024);
            assert_eq!(limits.max_output_tokens, 2048);
            self.0.complete_bounded(request, control)
        }
    }
    for specialist in [
        SpecialistId::DomainAnalystV8,
        SpecialistId::EvidenceAuditorV8,
    ] {
        let provider = Arc::new(PolicyCheckedProvider(Scripted::new(
            Mode::CompletionSupported,
        )));
        let fixture = CompletionFixture::new(provider.as_ref(), specialist, true, true, false);
        assert_eq!(fixture.request.schema_version, 5);
        assert_eq!(fixture.request.obligations.schema_version, 2);
        fixture.run(provider.clone());
        let state = fixture.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "error={:?}, usage={:?}",
            detail.error,
            detail.usage
        );
        assert_eq!(detail.limits, INVESTIGATION_V2_POLICY.limits());
        assert_eq!(detail.summary.specialist_id, specialist);
        assert_eq!(
            detail.summary.completion_outcome,
            Some(InvestigationCompletionOutcome::Supported)
        );
        assert_eq!(detail.usage.tool_actions, 3);
        assert_eq!(detail.usage.model_invocations, 4);
        assert_eq!(provider.0.calls.load(Ordering::SeqCst), 4);
        assert!(
            provider
                .0
                .prompts
                .lock()
                .unwrap()
                .iter()
                .all(|prompt| prompt["schema_version"] == 7
                    && prompt.get("inspection_requirements").is_none()
                    && prompt["limits"]["profile"] == "investigation-v2")
        );
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(result.schema_version, 5);
        assert!(
            result
                .completion
                .as_ref()
                .unwrap()
                .assessment
                .iter()
                .all(|item| item.exactly_supplied)
        );
        assert_eq!(
            jobs.investigation_completion_attempts(&fixture.id)
                .unwrap()
                .len(),
            2
        );
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result)
        );
    }
}
