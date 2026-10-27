//! AC-0287: the real worker crosses internal start, preparation, invocation,
//! response/refusal and terminal boundaries without a live provider.
use super::*;

struct NextFixture {
    base: CompletionFixture,
    request: StagedObligationRequest,
    id: String,
    execution: JobExecution,
    live: Arc<LiveTask>,
}
impl NextFixture {
    fn new(provider: &dyn LlmProvider, specialist: SpecialistId) -> Self {
        Self::with_source_obligation(provider, specialist, false)
    }
    fn with_source_obligation(
        provider: &dyn LlmProvider,
        specialist: SpecialistId,
        pinned: bool,
    ) -> Self {
        let base = CompletionFixture::new(
            provider,
            SpecialistId::DomainAnalystV9,
            pinned,
            !pinned,
            false,
        );
        let state = base.base.app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        // Reuse the real retained TS capture and qualification, retiring only
        // the unused old-family task created by the existing shared fixture.
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
        state.investigations.remove(&base.id);
        let mut request = base.request.clone();
        request.schema_version = 7;
        request.request_nonce = "next-prompt-worker-fixture".into();
        request.specialist_id = serde_json::to_value(specialist)
            .unwrap()
            .as_str()
            .unwrap()
            .into();
        let start = jobs
            .start_next_prompt_investigation(
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
        let lock = state
            .job_execution_locks
            .try_reserve(plan.lock_target())
            .unwrap();
        let (_, execution) = jobs.claim_execution(&plan, lock).unwrap();
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
            self.base.base.app.handle().clone(),
            self.id.clone(),
            self.request.clone(),
            self.execution.clone(),
            provider,
            self.live.clone(),
            Instant::now(),
        );
    }
}

#[derive(Clone, Copy)]
enum SourceScenario {
    Acquired,
    Unavailable,
    Corrupt,
    LostAck,
}

struct NextProvider {
    capacity_control: bool,
    observed: Scripted,
    malformed: bool,
    metadata_order: Option<[bool; 2]>,
    inventory_max_bytes: usize,
    lose_metadata_ack: bool,
    source_scenario: Option<SourceScenario>,
    extra_inventory: bool,
    large_finish: bool,
}
impl NextProvider {
    fn new(malformed: bool) -> Self {
        Self {
            capacity_control: false,
            observed: Scripted::new(Mode::CompletionNoAttempt),
            malformed,
            metadata_order: None,
            inventory_max_bytes: 32768,
            lose_metadata_ack: false,
            source_scenario: None,
            extra_inventory: false,
            large_finish: false,
        }
    }
}
impl LlmProvider for NextProvider {
    fn id(&self) -> &str {
        self.observed.id()
    }
    fn locality(&self) -> Locality {
        self.observed.locality()
    }
    fn capabilities(&self) -> ProviderCaps {
        self.observed.capabilities()
    }
    fn embed(&self, text: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        self.observed.embed(text)
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.observed.bounded_profile()
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        assert_eq!(prompt["schema_version"], 9);
        self.observed.prompts.lock().unwrap().push(prompt);
        let call = self.observed.calls.fetch_add(1, Ordering::SeqCst);
        // A real elapsed interval inside each scripted call witnesses that
        // the next protected pre-call sample comes from the running clock.
        std::thread::sleep(Duration::from_millis(40));
        if call == 0 && self.lose_metadata_ack {
            crate::jobs::investigations::metadata_host_fixture::lose_next_ack();
        }
        let text = if self.capacity_control {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            if call == 0 {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                    "max_facts":1,"max_bytes":32768,"cursor":null}})
                .to_string()
            } else {
                let option = &prompts[1]["compact_context"]["evidence_options"][0];
                let range = option["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|range| range["role"] == "definition_initializer")
                    .unwrap();
                json!({"type":"read_evidence","fact":option["fact"],"role":range["role"],"index":range["index"]}).to_string()
            }
        } else if let Some(scenario) = self.source_scenario {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            if call == 0 {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                    "max_facts":1,"max_bytes":32768,"cursor":null}})
                .to_string()
            } else if call == 1 {
                if matches!(scenario, SourceScenario::LostAck) {
                    crate::jobs::investigations::fail_next_source_publication(true);
                }
                let option = &prompt["compact_context"]["evidence_options"][0];
                let range = option["ranges"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|range| range["role"] == "definition_initializer")
                    .unwrap();
                json!({"type":"read_evidence","fact":option["fact"],"role":range["role"],"index":range["index"]}).to_string()
            } else if call == 2 && self.extra_inventory {
                let target =
                    &prompt["completion_contract"]["obligations"]["obligations"][0]["target"];
                json!({"type":"read_coverage_inventory","context_id":target["context_id"],
                    "repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768}).to_string()
            } else if matches!(scenario, SourceScenario::Unavailable) {
                json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
                    "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                    "limitations":["The actual retained source read was unavailable."]}})
                .to_string()
            } else {
                let citation = prompt["citations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                    .unwrap();
                let findings = (0..if self.large_finish { 12 } else { 1 }).map(|index| json!({
                    "claim_kind":"inferred_interpretation", "title":format!("Observed bounded guard {index}"),
                    "statement":if self.large_finish { "Observed source evidence. ".repeat(72) } else { "The acquired callable includes a conditional exit.".into() },
                    "citation_ids":[citation["citation_id"]],"limitations":["Runtime inputs remain unknown."]
                })).collect::<Vec<_>>();
                json!({"schema_version":1,"outcome":"supported","action":{
                    "type":"finish","findings":findings,
                    "knowledge_completeness":"partial","limitations":["No complete business policy was established."]}}).to_string()
            }
        } else if let Some(order) = self.metadata_order.filter(|_| call < 2) {
            let prompt = self.observed.prompts.lock().unwrap();
            let prompt = prompt.last().unwrap();
            if order[call] {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                    "max_facts":1,"max_bytes":32768,"cursor":null}})
                .to_string()
            } else {
                let inventory =
                    &prompt["completion_contract"]["obligations"]["obligations"][0]["target"];
                json!({"type":"read_coverage_inventory","context_id":inventory["context_id"],
                    "repo":inventory["repo"],"inventory_hash":inventory["inventory_hash"],"max_bytes":self.inventory_max_bytes}).to_string()
            }
        } else if self.malformed {
            "```json\n{}\n```".into()
        } else {
            json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
                "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                "limitations":["No inventory acquisition has completed."]}})
            .to_string()
        };
        if call == 2
            && let Some(cancel) = self.observed.cancel.lock().unwrap().as_ref()
        {
            // The final response has been computed while the invocation is in
            // flight. A late durable cancel must not rebase that valid finish.
            cancel();
            assert_eq!((control.check)(), CallDirective::Cancelled);
        }
        Ok(BoundedCompletion {
            text,
            requested_model: "scripted-model".into(),
            response_model: Some("scripted-model-observed".into()),
            provider_request_id: None,
            usage: Some(ReportedUsage {
                input_tokens: Some(100),
                output_tokens: Some(40),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}

#[test]
fn next_prompt_worker_real_start_invoke_refusal_feedback_and_terminal_reopen() {
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
    ] {
        let provider = Arc::new(NextProvider::new(false));
        let f = NextFixture::new(provider.as_ref(), role);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Failed,
            "{:?}",
            detail.error
        );
        assert_eq!(detail.summary.schema_version, 7);
        assert_eq!(detail.summary.specialist_id, role);
        assert_eq!(
            detail.summary.completion_outcome,
            Some(InvestigationCompletionOutcome::ObligationsUnmet)
        );
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 2);
        assert_eq!(detail.usage.model_invocations, 2);
        assert_eq!(detail.usage.tool_actions, 0);
        assert_eq!(detail.usage.evidence_requests, 0);
        assert!(!detail.summary.has_result);
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
        let prompts = provider.observed.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0]["completion_metadata"]["refusal_count"], 0);
        assert_eq!(prompts[1]["completion_metadata"]["refusal_count"], 1);
        assert_eq!(prompts[1]["turn_progress"]["capacity_refusal_count"], 0);
        assert_eq!(prompts[0]["context_id"], prompts[1]["context_id"]);
        let first_ms = prompts[0]["consumed_budgets"]["active_milliseconds"]
            .as_u64()
            .unwrap();
        let second_ms = prompts[1]["consumed_budgets"]["active_milliseconds"]
            .as_u64()
            .unwrap();
        assert!(
            second_ms >= first_ms + 40,
            "pre-call clock did not include first call"
        );
        assert!(
            detail.usage.active_milliseconds >= second_ms + 40,
            "terminal usage dropped elapsed call time"
        );
        assert_eq!(jobs.investigation(&f.id).unwrap(), detail);
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_owned_detail(&f.execution, &f.id)
                .unwrap(),
            detail
        );
        assert_eq!(
            serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
            f.base.base.graph_before
        );
    }
}

#[test]
fn next_prompt_worker_invalid_response_stops_after_one_actual_invocation() {
    let provider = Arc::new(NextProvider::new(true));
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Failed,
        "{:?}",
        detail.error
    );
    assert_eq!(detail.usage.model_invocations, 1);
    assert_eq!(detail.usage.tool_actions, 0);
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 0);
    assert!(!detail.summary.has_result);
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn next_prompt_worker_metadata_query_inventory_atomic_continuation_and_result7() {
    for order in [[true, false], [false, true], [false, false]] {
        let mut scripted = NextProvider::new(false);
        scripted.metadata_order = Some(order);
        let provider = Arc::new(scripted);
        let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "order={order:?}, error={:?}",
            detail.error
        );
        assert_eq!(
            detail.summary.completion_outcome,
            Some(InvestigationCompletionOutcome::InsufficientEvidence)
        );
        assert!(detail.summary.has_result);
        assert_eq!(detail.usage.model_invocations, 3);
        assert_eq!(detail.usage.tool_actions, 2);
        assert_eq!(detail.usage.evidence_requests, 0);
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 0);
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
        let prompts = provider.observed.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3);
        assert_eq!(
            prompts[2]["coverage_details"].as_array().unwrap().len(),
            order.iter().filter(|query| !**query).count()
        );
        assert_eq!(
            prompts[2]["completion_metadata"]["completed_attempt_ids"]
                .as_array()
                .unwrap()
                .len(),
            order.iter().filter(|query| !**query).count()
        );
        assert_eq!(prompts[2]["consumed_budgets"]["tool_actions"], 2);
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_owned_detail(&f.execution, &f.id)
                .unwrap(),
            detail
        );
        // Read actual immutable bodies, without bypassing the owner validator
        // above or making a public next-family history route available.
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let body: Vec<u8> = connection
            .query_row(
                "SELECT body FROM investigation_results WHERE task_id=?1",
                [&f.id],
                |row| row.get(0),
            )
            .unwrap();
        let result: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(result["schema_version"], 7);
        assert!(
            result["disclosure_hash"]
                .as_str()
                .unwrap()
                .starts_with("investigation-disclosure-v6:")
        );
        let inputs: i64 = connection
            .query_row(
                "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
                [&f.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            inputs, 3,
            "one initial plus exactly two atomic acquisitions"
        );
    }
}

#[test]
fn next_prompt_worker_metadata_lost_ack_and_read_limit_never_retry_or_fabricate_capacity_feedback()
{
    for lost_ack in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.metadata_order = Some([false, false]);
        scripted.inventory_max_bytes = if lost_ack { 32768 } else { 1 };
        scripted.lose_metadata_ack = lost_ack;
        let provider = Arc::new(scripted);
        let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Failed,
            "lost_ack={lost_ack}, error={:?}",
            detail.error
        );
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 1);
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(detail.usage.tool_actions, u32::from(lost_ack));
        assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 0);
        assert!(!detail.summary.has_result);
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let inputs: i64 = connection
            .query_row(
                "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
                [&f.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(inputs, if lost_ack { 2 } else { 1 });
        let mut statement = connection
            .prepare("SELECT body FROM investigation_events WHERE task_id=?1")
            .unwrap();
        let bodies = statement
            .query_map([&f.id], |row| row.get::<_, Vec<u8>>(0))
            .unwrap();
        for body in bodies {
            let event: Value = serde_json::from_slice(&body.unwrap()).unwrap();
            assert_ne!(event["kind"], "capacity_refused");
        }
    }
}

#[test]
fn next_prompt_worker_source_acquisition_and_unavailable_use_real_read_and_result7() {
    for unavailable in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.source_scenario = Some(if unavailable {
            SourceScenario::Unavailable
        } else {
            SourceScenario::Acquired
        });
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV10,
            true,
        );
        if unavailable {
            rusqlite::Connection::open(
                f.base.base.app_data.join("retained-source/captures.sqlite"),
            )
            .unwrap()
            .execute("DELETE FROM objects", [])
            .unwrap();
        }
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Completed,
            "unavailable={unavailable}, error={:?}",
            detail.error
        );
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
        assert_eq!(detail.usage.tool_actions, 2);
        assert_eq!(detail.usage.evidence_requests, 1);
        assert_eq!(detail.usage.evidence_items, usize::from(!unavailable));
        assert!(detail.usage.captured_validation_bytes > 0);
        assert!(detail.summary.has_result);
        assert_eq!(
            detail.summary.completion_outcome,
            Some(if unavailable {
                InvestigationCompletionOutcome::InsufficientEvidence
            } else {
                InvestigationCompletionOutcome::Supported
            })
        );
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened
                .investigation_owned_detail(&f.execution, &f.id)
                .unwrap(),
            detail
        );
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let body: Vec<u8> = connection
            .query_row(
                "SELECT body FROM investigation_results WHERE task_id=?1",
                [&f.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["schema_version"],
            7
        );
    }
}

#[test]
fn next_prompt_worker_source_corrupt_and_unknown_publication_ack_do_not_read_again() {
    for lost_ack in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.source_scenario = Some(if lost_ack {
            SourceScenario::LostAck
        } else {
            SourceScenario::Corrupt
        });
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV10,
            true,
        );
        if !lost_ack {
            rusqlite::Connection::open(
                f.base.base.app_data.join("retained-source/captures.sqlite"),
            )
            .unwrap()
            .execute("UPDATE objects SET bytes=X'00'", [])
            .unwrap();
        }
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        assert_eq!(
            detail.summary.status,
            InvestigationStatus::Failed,
            "lost_ack={lost_ack}, error={:?}",
            detail.error
        );
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
        assert_eq!(detail.usage.tool_actions, 2);
        assert_eq!(detail.usage.evidence_requests, 1);
        assert_eq!(detail.usage.evidence_items, usize::from(lost_ack));
        assert!(detail.usage.captured_validation_bytes > 0);
        assert!(!detail.summary.has_result);
    }
}

#[test]
fn next_prompt_worker_queried_source_outside_obligations_can_acquire_and_finish() {
    let mut scripted = NextProvider::new(false);
    scripted.source_scenario = Some(SourceScenario::Acquired);
    scripted.extra_inventory = true;
    let provider = Arc::new(scripted);
    // The sole immutable obligation is inventory. The model learns the source
    // selector only from its paid query; no source target is in the request.
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&f.execution, &f.id)
        .unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
    assert_eq!(detail.usage.evidence_items, 1);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(detail.usage.tool_actions, 3);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::Supported)
    );
}

#[path = "next_prompt_history_tests.rs"]
mod history_tests;

#[path = "next_prompt_wire_export_tests.rs"]
mod wire_export;

// Retain the genuine parser capture and app for public command tests without
// exporting fixture internals or creating an already-admitted next-family task.
pub(in crate::investigations) fn with_public_start_fixture(
    provider: Arc<dyn LlmProvider>,
    legacy_completed: bool,
    test: impl FnOnce(
        &tauri::AppHandle<tauri::test::MockRuntime>,
        StagedObligationRequest,
        &std::path::Path,
    ),
) {
    with_public_start_fixture_then(provider, legacy_completed, test, |(), _| {});
}

pub(in crate::investigations) fn with_public_start_fixture_then<T>(
    provider: Arc<dyn LlmProvider>,
    legacy_completed: bool,
    test: impl FnOnce(
        &tauri::AppHandle<tauri::test::MockRuntime>,
        StagedObligationRequest,
        &std::path::Path,
    ) -> T,
    after_drop: impl FnOnce(T, &std::path::Path),
) {
    let mut fixture = CompletionFixture::new(
        provider.as_ref(),
        SpecialistId::DomainAnalystV9,
        true,
        false,
        false,
    );
    let mut request = fixture.request.clone();
    if legacy_completed {
        fixture.run(provider);
        assert_eq!(
            fixture
                .base
                .app
                .state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .investigation(&fixture.id)
                .unwrap()
                .summary
                .status,
            InvestigationStatus::Completed
        );
    } else {
        let state = fixture.base.app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        jobs.cancel_investigation(&fixture.id).unwrap();
        let revision = jobs.investigation(&fixture.id).unwrap().summary.revision;
        jobs.advance_investigation(
            &fixture.execution,
            revision,
            InvestigationTransition::StopWithElapsed {
                reason: InvestigationStopReason::Cancelled,
                active_milliseconds: 0,
            },
        )
        .unwrap();
        state.investigations.remove(&fixture.id);
        request.schema_version = 7;
        request.specialist_id = "domain-analyst@10".into();
        request.request_nonce = "public-command-activation-fixture".into();
    }
    let result = test(fixture.base.app.handle(), request, &fixture.base.app_data);
    let app_data = fixture.base.app_data.clone();
    // Keep only the disposable directory, dropping every old host/store owner.
    let directory = std::mem::replace(&mut fixture.base._directory, tempfile::tempdir().unwrap());
    drop(fixture);
    after_drop(result, &app_data);
    drop(directory);
}

// AC-0187, AC-0287: cancellation during a dispatched completion preserves a
// valid finish bound to its actual supplied input, without another acquisition.
#[test]
fn next_prompt_worker_late_inflight_cancel_retains_valid_finish_against_original_input() {
    let mut scripted = NextProvider::new(false);
    scripted.source_scenario = Some(SourceScenario::Acquired);
    let provider = Arc::new(scripted);
    let fixture = NextFixture::with_source_obligation(
        provider.as_ref(),
        SpecialistId::DomainAnalystV10,
        true,
    );
    let app = fixture.base.base.app.handle().clone();
    let id = fixture.id.clone();
    let cancelled_basis = Arc::new(Mutex::new(None));
    let observed_basis = cancelled_basis.clone();
    *provider.observed.cancel.lock().unwrap() = Some(Box::new(move || {
        let state = app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        let before = jobs.investigation(&id).unwrap();
        assert_eq!(before.summary.status, InvestigationStatus::Running);
        assert_eq!(before.usage.model_invocations, 3);
        assert_eq!(before.usage.tool_actions, 2);
        assert_eq!(before.usage.evidence_requests, 1);
        let ledger = jobs.investigation_ledger(&id).unwrap().unwrap();
        let cancelled = jobs.cancel_next_prompt_investigation(&id).unwrap();
        assert!(cancelled.summary.cancel_requested);
        assert_eq!(cancelled.summary.revision, before.summary.revision + 1);
        assert_eq!(
            cancelled.summary.last_event_sequence,
            before.summary.last_event_sequence + 1
        );
        assert_eq!(cancelled.summary.status, before.summary.status);
        assert!(before.summary.invocation_pending && cancelled.summary.invocation_pending);
        assert_eq!(cancelled.usage, before.usage);
        assert_eq!(cancelled.citations, before.citations);
        assert!(
            observed_basis
                .lock()
                .unwrap()
                .replace((before.usage, ledger))
                .is_none()
        );
    }));
    fixture.run(provider.clone());
    let state = fixture.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs
        .investigation_owned_detail(&fixture.execution, &fixture.id)
        .unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert!(detail.summary.cancel_requested);
    assert_eq!(
        detail.summary.completion_outcome,
        Some(InvestigationCompletionOutcome::Supported)
    );
    assert!(detail.summary.has_result);
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
    assert_eq!(provider.observed.prompts.lock().unwrap().len(), 3);
    assert_eq!(detail.usage.model_invocations, 3);
    assert_eq!(detail.usage.reported_input_tokens, Some(300));
    assert_eq!(detail.usage.reported_output_tokens, Some(120));
    assert_eq!(detail.usage.tool_actions, 2);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(detail.usage.evidence_items, 1);
    assert_eq!(detail.completion.as_ref().unwrap().refusal_count, 0);
    let (before, ledger) = cancelled_basis
        .lock()
        .unwrap()
        .take()
        .expect("cancel ran inside final call");
    assert_eq!(
        detail.usage.captured_validation_bytes,
        before.captured_validation_bytes
    );
    assert_eq!(detail.usage.evidence_bytes, before.evidence_bytes);
    assert_eq!(
        jobs.investigation_ledger(&fixture.id).unwrap().unwrap(),
        ledger
    );
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(result.schema_version, 7);
    assert_eq!(result.invocation_step_id.as_deref(), Some("step-3"));
    assert_eq!(
        result.input_ledger_hash,
        ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
    );
    assert_eq!(result.graph_snapshot_id, ledger.graph_snapshot_id);
    assert_eq!(
        provider.observed.prompts.lock().unwrap()[2]["input_ledger_hash"],
        result.input_ledger_hash
    );
    let connection =
        rusqlite::Connection::open(fixture.base.base.app_data.join("state.db")).unwrap();
    let invocation: Vec<u8> = connection
        .query_row(
            "SELECT body FROM investigation_invocations WHERE task_id=?1 AND step_id='step-3'",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    let invocation: PendingInvestigationStep = serde_json::from_slice(&invocation).unwrap();
    result
        .validate_with_disclosure_and_policy(
            &ledger,
            "step-3",
            invocation.disclosure.as_ref().unwrap(),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    for (table, count) in [
        ("investigation_invocations", 3),
        ("investigation_responses", 3),
        ("completion_attempts", 1),
        ("investigation_read_reservations", 1),
    ] {
        let saved: i64 = connection
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
                [&fixture.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            saved, count,
            "{table}: no retry or acquisition after late cancel"
        );
    }
    let events = jobs.investigation_events(&fixture.id, 0).unwrap();
    assert!(!events.has_more);
    assert!(!events.items.iter().any(|event| matches!(
        event.kind,
        InvestigationEventKind::FinishRefused | InvestigationEventKind::CapacityRefused
    )));
    let cancel = events
        .items
        .iter()
        .position(|event| event.kind == InvestigationEventKind::CancelRequested)
        .unwrap();
    assert!(events.items[cancel + 1..].iter().all(|event| !matches!(
        event.kind,
        InvestigationEventKind::ModelStarted
            | InvestigationEventKind::ToolStarted
            | InvestigationEventKind::EvidenceValidationReserved
    )));
    assert!(
        events.items[cancel + 1..]
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ResultPersisted)
    );
    assert_eq!(jobs.investigation(&fixture.id).unwrap(), detail);
    drop(jobs);
    let reopened =
        crate::jobs::JobStore::open(fixture.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap().unwrap(),
        result
    );
    assert_eq!(
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
        fixture.base.base.graph_before
    );
}

// AC-0327: the detached execution handoff retains host evidence caps.
#[test]
fn next_prompt_worker_preserves_source_file_and_span_caps() {
    for refused in [None, Some("file"), Some("span")] {
        let mut scripted = NextProvider::new(false);
        scripted.source_scenario = Some(SourceScenario::Acquired);
        let provider = Arc::new(scripted);
        let f = NextFixture::with_source_obligation(
            provider.as_ref(),
            SpecialistId::DomainAnalystV10,
            true,
        );
        let target = f
            .request
            .obligations
            .sources_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let file = target.captured.file.byte_len;
        let span = target.captured.byte_end - target.captured.byte_start;
        let mut limits = crate::mcp_reads::worker::tests::task_read_limits();
        limits.evidence.max_file_validation_bytes =
            std::num::NonZeroU64::new(file - u64::from(refused == Some("file"))).unwrap();
        limits.evidence.max_span_bytes =
            std::num::NonZeroU64::new(span - u64::from(refused == Some("span"))).unwrap();
        run_completion_with_limits(
            f.base.base.app.handle().clone(),
            f.id.clone(),
            f.request.clone(),
            f.execution.clone(),
            provider.clone(),
            f.live.clone(),
            Instant::now(),
            Some(limits),
        );
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs
            .investigation_owned_detail(&f.execution, &f.id)
            .unwrap();
        if refused.is_none() {
            assert_eq!(
                detail.summary.status,
                InvestigationStatus::Completed,
                "{:?}",
                detail.error
            );
            assert_eq!(detail.usage.evidence_items, 1);
            assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
        } else {
            assert!(terminal(detail.summary.status));
            assert_eq!(
                detail.error.as_deref(),
                Some("An investigation work or storage limit was reached.")
            );
            assert_eq!(detail.usage.evidence_requests, 0);
            assert_eq!(detail.usage.captured_validation_bytes, 0);
            assert_eq!(detail.usage.evidence_items, 0);
            assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
        }
    }
}

// Reuse the production-worker scripted fixture for the staged analyst harness.
pub(in crate::investigations) fn expanded_scripted_provider(partial: bool) -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    if partial {
        provider.source_scenario = Some(SourceScenario::Acquired);
        provider.extra_inventory = true;
    }
    Arc::new(provider)
}

pub(in crate::investigations) fn expanded_capacity_provider() -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.capacity_control = true;
    Arc::new(provider)
}
