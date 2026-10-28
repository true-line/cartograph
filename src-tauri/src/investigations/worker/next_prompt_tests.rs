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
        self.base.base.app.run_completion(
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
    metadata_query: Option<Value>,
    received_payloads: std::sync::Mutex<Vec<llm::CompletionPayload>>,
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
            metadata_query: None,
            received_payloads: std::sync::Mutex::new(Vec::new()),
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
        if self.metadata_query.is_some() {
            self.received_payloads
                .lock()
                .unwrap()
                .push(request.payload().clone());
        }
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
        } else if let Some(order) = self
            .metadata_order
            .filter(|_| call < if self.malformed { 1 } else { 2 })
        {
            let prompt = self.observed.prompts.lock().unwrap();
            let prompt = prompt.last().unwrap();
            if order[call] {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":self.metadata_query.clone().unwrap_or_else(|| json!({
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                    "max_facts":1,"max_bytes":32768,"cursor":null}))})
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
    // AC-0439: new disposable @12 task uses the canonical first discovery query.
    for (role, order) in [
        (SpecialistId::DomainAnalystV10, [true, false]),
        (SpecialistId::DomainAnalystV10, [false, true]),
        (SpecialistId::DomainAnalystV10, [false, false]),
        (SpecialistId::DomainAnalystV12, [true, false]),
    ] {
        let mut scripted = NextProvider::new(false);
        scripted.metadata_order = Some(order);
        if role == SpecialistId::DomainAnalystV12 {
            scripted.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
                "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
        }
        let provider = Arc::new(scripted);
        let f = NextFixture::new(provider.as_ref(), role);
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
        if order[0] {
            let body: Vec<u8> = connection.query_row(
                "SELECT body FROM investigation_inputs WHERE task_id=?1 ORDER BY revision LIMIT 1 OFFSET 1",
                [&f.id], |row| row.get(0)).unwrap();
            let ledger: InvestigationInputLedger = serde_json::from_slice(&body).unwrap();
            assert_eq!(ledger.queries.len(), 1, "first query durably published");
            let second: Vec<u8> = connection.query_row(
                "SELECT body FROM investigation_invocations WHERE task_id=?1 AND step_id='step-2'",
                [&f.id], |row| row.get(0)).unwrap();
            let second: Value = serde_json::from_slice(&second).unwrap();
            if role == SpecialistId::DomainAnalystV12 {
                assert_canonical_query_continuation(
                    &provider.received_payloads.lock().unwrap()[1],
                    &ledger,
                    &f.request
                        .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap(),
                    second["input_bytes"].as_u64().unwrap() as usize,
                    8,
                );
            }
        }
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
    // This callback can dispatch detached workers or return cloned handles.
    // Opt out before even the optional inline run; its return cannot re-enable release.
    fixture.base.app.retain_for_external_use();
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
    // Keep the disposable directory while dropping the fixture owner. Tauri and
    // detached workers may still retain managed state; this is not a worker join.
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
                Some("Investigation limit reached: host_limit.")
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

fn assert_canonical_query_continuation(
    payload: &llm::CompletionPayload,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
    saved_bytes: usize,
    expected_count: usize,
) {
    let prompt: Value = serde_json::from_str(&payload.prompt).unwrap();
    let compact: ProgressiveInvestigationContext =
        serde_json::from_value(prompt["compact_context"].clone()).unwrap();
    compact
        .query_history
        .validate_registry(request_hash, ledger, &INVESTIGATION_V2_POLICY)
        .unwrap();
    let latest = compact.query_history.latest_query.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(&latest.selection).unwrap(),
        json!({"scope":{"type":"all"},"kind":null,
        "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null})
    );
    assert_eq!(latest.context.fact_indexes.len(), expected_count);
    assert_eq!(compact.facts.len(), expected_count);
    assert_eq!(latest.evidence_option_indexes.len(), expected_count);
    assert_eq!(ledger.selected_facts.len(), expected_count);
    let expected: std::collections::BTreeSet<_> = ledger
        .selected_facts
        .iter()
        .map(|selected| serde_json::to_string(&selected.fact).unwrap())
        .collect();
    let actual: std::collections::BTreeSet<_> = latest
        .context
        .fact_indexes
        .iter()
        .map(|&index| {
            let fact: context_hub::ContextFact =
                serde_json::from_value(compact.facts[index].clone()).unwrap();
            serde_json::to_string(&fact.reference).unwrap()
        })
        .collect();
    assert_eq!(actual, expected);
    // Exact complete payload encoding, not a guessed lower bound or fixed size.
    assert_eq!(saved_bytes, serde_json::to_vec(payload).unwrap().len());
}

// AC-0439: one canonical query, then a deliberate malformed terminal control.
pub(in crate::investigations) fn expanded_query_repro_provider() -> (
    Arc<dyn LlmProvider>,
    impl Fn(&InvestigationInputLedger, &str, usize),
) {
    let mut provider = NextProvider::new(true);
    provider.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
        "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
    // Only the first call uses metadata_order; subsequent calls are malformed.
    provider.metadata_order = Some([true, false]);
    let provider = Arc::new(provider);
    let observed = provider.clone();
    (provider, move |ledger, hash, bytes| {
        let payloads = observed.received_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 2);
        assert_canonical_query_continuation(&payloads[1], ledger, hash, bytes, 12);
    })
}

pub(in crate::investigations) fn expanded_capacity_provider() -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.capacity_control = true;
    Arc::new(provider)
}

// AC-0430, AC-0436: rejection stages survive reopen without retaining rejected content.
#[test]
fn next_prompt_response_rejection_stages_are_content_free_and_durable() {
    use agents::investigation::{
        ActionDecodeFailure, ActionDecodeStage as DecodeStage, ActionSemanticRule as Rule,
        InvestigationError,
    };
    const SENTINEL: &str = "PRIVATE-REJECTED-RESPONSE-1266";
    struct Rejected {
        profile: Scripted,
        calls: AtomicUsize,
        stage: InvestigationStopReason,
        text: String,
    }
    impl LlmProvider for Rejected {
        fn id(&self) -> &str {
            self.profile.id()
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> ProviderCaps {
            self.profile.capabilities()
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
            Err(ProviderError::Unsupported("fixture embeddings"))
        }
        fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
            self.profile.bounded_profile()
        }
        fn complete_bounded(
            &self,
            _: &AuthorizedBoundedCompletion,
            _: &CompletionControl<'_>,
        ) -> Result<BoundedCompletion, BoundedCallError> {
            assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
            if let InvestigationStopReason::BoundedProvider(error) = self.stage {
                return Err(error);
            }
            Ok(BoundedCompletion {
                text: self.text.clone(),
                requested_model:
                    if self.stage == InvestigationStopReason::InvalidCompletionEnvelope
                        && self.text.len() <= agents::investigation::MAX_ACTION_BYTES
                    {
                        "wrong-model"
                    } else {
                        "scripted-model"
                    }
                    .into(),
                response_model: Some(
                    if self.stage == InvestigationStopReason::InvalidResponseMetadata {
                        format!("\0{SENTINEL}")
                    } else {
                        "scripted-model-observed".into()
                    },
                ),
                provider_request_id: Some(SENTINEL.into()),
                usage: None,
                request_bytes: 100,
                response_bytes: 100,
            })
        }
    }
    let mut messages = std::collections::BTreeSet::new();
    let oversized = SENTINEL.repeat(agents::investigation::MAX_ACTION_BYTES / SENTINEL.len() + 1);
    // The decoder's byte-limit class exists, but the real worker envelope
    // rejects this size first. Do not bypass that earlier production gate.
    assert!(matches!(
        CompletionInvestigationAction::decode(&oversized, ""),
        Err(InvestigationError::LimitExceeded)
    ));
    assert_eq!(
        InvestigationStopReason::InvalidNextAction(InvestigationError::LimitExceeded)
            .test_message(),
        "Action decode refused: limit_exceeded."
    );
    for error in [
        InvestigationError::InvalidRequest,
        InvestigationError::InvalidAction,
        InvestigationError::LimitExceeded,
        InvestigationError::InvalidInput,
        InvestigationError::InvalidCitation,
        InvestigationError::SourceReplay,
        InvestigationError::SensitiveOutput,
    ] {
        for stage in [
            DecodeStage::Preflight,
            DecodeStage::TypedEnvelope,
            DecodeStage::CanonicalFields,
            DecodeStage::Semantic,
            DecodeStage::HostBinding,
            DecodeStage::SemanticRule(Rule::QueryContext),
            DecodeStage::SemanticRule(Rule::QueryShape),
            DecodeStage::SemanticRule(Rule::InventoryContext),
            DecodeStage::SemanticRule(Rule::InventoryShape),
            DecodeStage::SemanticRule(Rule::ReplayContext),
            DecodeStage::SemanticRule(Rule::ReplayReference),
            DecodeStage::SemanticRule(Rule::EvidenceShape),
            DecodeStage::SemanticRule(Rule::FinishRole),
            DecodeStage::SemanticRule(Rule::FinishShape),
            DecodeStage::SemanticRule(Rule::FinishOutcome),
        ] {
            let reason =
                InvestigationStopReason::ActionDecode(ActionDecodeFailure { stage, error });
            assert!(
                reason.test_message().len()
                    <= InvestigationStopReason::InvalidResponse
                        .test_message()
                        .len()
            );
            assert_eq!(
                reason.terminal_class(),
                InvestigationStopReason::InvalidNextAction(error)
            );
        }
        assert!(
            InvestigationStopReason::InvalidNextAction(error)
                .test_message()
                .len()
                <= InvestigationStopReason::InvalidResponse
                    .test_message()
                    .len()
        );
    }
    let decoded =
        |stage, error| InvestigationStopReason::ActionDecode(ActionDecodeFailure { stage, error });
    let bounded = |code, outcome| {
        (
            InvestigationStopReason::BoundedProvider(BoundedCallError { code, outcome }),
            SENTINEL.into(),
        )
    };
    for (stage, text) in [
        (
            InvestigationStopReason::InvalidCompletionEnvelope,
            SENTINEL.into(),
        ),
        (
            InvestigationStopReason::InvalidResponseMetadata,
            SENTINEL.into(),
        ),
        (
            decoded(DecodeStage::Preflight, InvestigationError::InvalidInput),
            SENTINEL.into(),
        ),
        (
            decoded(DecodeStage::TypedEnvelope, InvestigationError::InvalidAction),
            json!({"type":"unsupported_action", "private":SENTINEL}).to_string(),
        ),
        (
            decoded(DecodeStage::SemanticRule(Rule::QueryContext), InvestigationError::InvalidAction),
            json!({"type":"query_context", "context_id":format!("context-v2:{}", "a".repeat(64)),
                "query":{"scope":{"type":"all"},"kind":null,"labels":[SENTINEL],
                    "max_facts":1,"max_bytes":1024,"cursor":null}})
            .to_string(),
        ),
        (decoded(DecodeStage::CanonicalFields, InvestigationError::InvalidAction),
            json!({"type":"query_context","context_id":"wrong",
                "query":{"scope":{"type":"all"},"labels":[SENTINEL],"max_facts":1,"max_bytes":1024,"cursor":null}}).to_string()),
        (
            InvestigationStopReason::InvalidCompletionEnvelope,
            oversized,
        ),
        bounded(
            FailureCode::ResponseTooLarge,
            InvocationOutcome::ResponseReceived,
        ),
        bounded(FailureCode::Transport, InvocationOutcome::Unknown),
        bounded(FailureCode::Cancelled, InvocationOutcome::Unknown),
        bounded(FailureCode::Cancelled, InvocationOutcome::ResponseReceived),
        bounded(
            FailureCode::OwnershipLost,
            InvocationOutcome::ResponseReceived,
        ),
    ] {
        let provider = Arc::new(Rejected {
            profile: Scripted::new(Mode::Malformed),
            calls: AtomicUsize::new(0),
            stage,
            text: text.clone(),
        });
        let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV10);
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&f.id).unwrap();
        let expected_status = match stage.terminal_class() {
            InvestigationStopReason::OutcomeUnknown => InvestigationStatus::OutcomeUnknown,
            InvestigationStopReason::Cancelled => InvestigationStatus::Cancelled,
            _ => InvestigationStatus::Failed,
        };
        assert_eq!(detail.summary.status, expected_status);
        assert_eq!(
            detail.summary.cancel_requested,
            expected_status == InvestigationStatus::Cancelled
        );
        assert_eq!(detail.error.as_deref(), Some(stage.test_message()));
        messages.insert(stage.test_message());
        assert!(
            stage.test_message().len()
                <= InvestigationStopReason::InvalidResponse
                    .test_message()
                    .len()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(detail.usage.tool_actions, 0);
        assert_eq!(detail.usage.evidence_items, 0);
        assert!(jobs.investigation_result(&f.id).unwrap().is_none());
        let events = jobs.investigation_events(&f.id, 0).unwrap();
        assert!(!events.has_more);
        assert!(
            serde_json::to_string(&events)
                .unwrap()
                .contains(stage.test_message())
        );
        drop(jobs);
        let db = f.base.base.app_data.join("state.db");
        let reopened = crate::jobs::JobStore::open(&db).unwrap();
        assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
        assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
        assert!(reopened.investigation_result(&f.id).unwrap().is_none());
        // Include SQLite's WAL: rejected response bytes and their digest must
        // not be retained even in pages outside the current logical rows.
        for path in [db.clone(), db.with_file_name("state.db-wal")] {
            if path.exists() {
                let bytes = std::fs::read(path).unwrap();
                for forbidden in [SENTINEL.to_owned(), content_hash(text.as_bytes())] {
                    assert!(
                        !bytes
                            .windows(forbidden.len())
                            .any(|b| b == forbidden.as_bytes())
                    );
                }
            }
        }
    }
    assert_eq!(messages.len(), 11);
}

// AC-0432, AC-0439: explicit generations keep their saved definitions through actual worker refusal/reopen.
#[test]
fn progress_guidance_roles_preserve_saved_v10_and_v11_definitions() {
    for role in [
        SpecialistId::DomainAnalystV10,
        SpecialistId::EvidenceAuditorV10,
        SpecialistId::DomainAnalystV11,
        SpecialistId::DomainAnalystV12,
        SpecialistId::EvidenceAuditorV11,
    ] {
        let provider = Arc::new(Scripted::new(Mode::Malformed));
        let f = NextFixture::new(provider.as_ref(), role);
        f.run(provider);
        let state = f.base.base.app.state::<AppState>();
        let detail = state.jobs.lock().unwrap().investigation(&f.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        assert_eq!(detail.summary.schema_version, 7);
        assert_eq!(detail.specialist, role.definition().unwrap());
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(detail.usage.tool_actions, 0);
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    }
}

// AC-0440: only Host InvalidInput gains a phase; outer routing cannot replace it.
#[test]
fn next_prompt_preparation_phases_preserve_failures() {
    use PreparationPhase::*;
    let mut messages = std::collections::BTreeSet::new();
    for phase in [
        Continuation,
        Finish,
        Metadata,
        Query,
        Inventory,
        Candidate,
        Certification,
        Seal,
        Publication,
        Source,
        SourcePrepare,
        SourceCertification,
        SourceReservation,
    ] {
        let annotated = Failure::Host(HostError::InvalidInput).preparation_at(phase);
        let reason = annotated.reason().unwrap();
        assert_eq!(reason, InvestigationStopReason::PreparationAt(phase));
        assert_eq!(
            reason.terminal_class(),
            InvestigationStopReason::PreparationFailed
        );
        // Compatibility baseline: the existing 61-byte InvalidResponse message,
        // not a separately claimed durable-column schema limit.
        assert!(
            reason.test_message().len()
                <= InvestigationStopReason::InvalidResponse
                    .test_message()
                    .len()
        );
        assert!(messages.insert(reason.test_message()));
        assert_eq!(
            annotated
                .preparation_at(Source)
                .preparation_after_call(SpecialistId::DomainAnalystV12, 1)
                .reason(),
            Some(reason)
        );
    }
    for original in [
        Failure::AlreadyTerminal,
        Failure::Host(HostError::LimitExceeded),
        Failure::Host(HostError::StoreCapacity),
        Failure::Host(HostError::Operational),
        Failure::Host(HostError::InputChanged),
        Failure::Host(HostError::SourceUnavailable),
        Failure::Store(InvestigationStoreError::Invalid),
        Failure::Store(InvestigationStoreError::Capacity),
        Failure::Store(InvestigationStoreError::Cancelled),
        Failure::Stop(InvestigationStopReason::Cancelled),
        Failure::Stop(InvestigationStopReason::ActionDecode(ActionDecodeFailure {
            stage: ActionDecodeStage::Semantic,
            error: InvestigationError::InvalidAction,
        })),
        Failure::Stop(InvestigationStopReason::BoundedProvider(BoundedCallError {
            code: FailureCode::Transport,
            outcome: InvocationOutcome::Unknown,
        })),
    ] {
        let before = format!("{original:?}");
        assert_eq!(
            format!(
                "{:?}",
                original
                    .preparation_at(Query)
                    .preparation_after_call(SpecialistId::DomainAnalystV12, 1)
            ),
            before
        );
    }
    for (role, calls, expected) in [
        (
            SpecialistId::DomainAnalystV12,
            0,
            InvestigationStopReason::PreparationFailed,
        ),
        (
            SpecialistId::DomainAnalystV12,
            1,
            InvestigationStopReason::PreparationAt(Continuation),
        ),
        (
            SpecialistId::DomainAnalystV7,
            1,
            InvestigationStopReason::PreparationFailed,
        ),
    ] {
        assert_eq!(
            Failure::Host(HostError::InvalidInput)
                .preparation_after_call(role, calls)
                .reason(),
            Some(expected)
        );
    }
    assert_eq!(
        InvestigationStopReason::PreparationFailed.test_message(),
        "Investigation input preparation failed."
    );
}

// AC-0440: a syntactically valid cursor can fail at the host query boundary.
#[test]
fn next_prompt_foreign_cursor_preparation_phase_is_durable() {
    let provider = Arc::new(Scripted::new(Mode::ForeignCursor));
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV12);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    let reason = InvestigationStopReason::PreparationAt(PreparationPhase::Query);
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(detail.error.as_deref(), Some(reason.test_message()));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(detail.usage.model_invocations, 1);
    assert_eq!(detail.usage.tool_actions, 0);
    assert_eq!(detail.usage.selected_facts, 0);
    assert_eq!(detail.usage.evidence_items, 0);
    assert!(jobs.investigation_result(&f.id).unwrap().is_none());
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert!(
        serde_json::to_string(&events)
            .unwrap()
            .contains(reason.test_message())
    );
    drop(jobs);
    let db = f.base.base.app_data.join("state.db");
    let reopened = crate::jobs::JobStore::open(&db).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
    for path in [db.clone(), db.with_file_name("state.db-wal")] {
        if path.exists() {
            assert!(
                !std::fs::read(path)
                    .unwrap()
                    .windows(b"PRIVATE-FOREIGN-CURSOR-1293".len())
                    .any(|bytes| bytes == b"PRIVATE-FOREIGN-CURSOR-1293")
            );
        }
    }
}
