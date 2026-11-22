//! AC-0287: the real worker crosses internal start, preparation, invocation,
//! response/refusal and terminal boundaries without a live provider.
use super::*;

/// The widest saved desktop active allowance, equal to the hard wall limit.
const WALL_BOUNDED_ACTIVE_SECONDS: u64 = 3600;
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
    /// A long scripted run on the real running clock, saved with the widest
    /// desktop active allowance so the hard wall limit is the only deadline.
    /// Debug-build host work, not the script, dominates elapsed time, and a
    /// slow shared runner otherwise crosses the 600 s default part-way through
    /// (#1896). The request timeout stays at the default the provider asserts.
    fn new_with_wall_bounded_active(provider: &dyn LlmProvider, specialist: SpecialistId) -> Self {
        let base =
            CompletionFixture::new(provider, SpecialistId::DomainAnalystV9, false, true, false);
        let request = base.request.clone();
        Self::from_completion_base_with_wall_bounded_active(provider, specialist, base, request)
    }
    fn from_completion_base_with_wall_bounded_active(
        provider: &dyn LlmProvider,
        specialist: SpecialistId,
        base: CompletionFixture,
        request: StagedObligationRequest,
    ) -> Self {
        Self::from_completion_base_with_limits(
            provider,
            specialist,
            base,
            request,
            Some((
                WALL_BOUNDED_ACTIVE_SECONDS,
                CompletionLimits::default().values().request_timeout_ms,
            )),
        )
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
        let request = base.request.clone();
        Self::from_completion_base(provider, specialist, base, request)
    }
    fn from_completion_base(
        provider: &dyn LlmProvider,
        specialist: SpecialistId,
        base: CompletionFixture,
        request: StagedObligationRequest,
    ) -> Self {
        Self::from_completion_base_with_limits(provider, specialist, base, request, None)
    }
    fn from_completion_base_with_limits(
        provider: &dyn LlmProvider,
        specialist: SpecialistId,
        base: CompletionFixture,
        mut request: StagedObligationRequest,
        configured: Option<(u64, u64)>,
    ) -> Self {
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
        request.schema_version = 7;
        request.request_nonce = "next-prompt-worker-fixture".into();
        request.specialist_id = serde_json::to_value(specialist)
            .unwrap()
            .as_str()
            .unwrap()
            .into();
        let mode = match provider.locality() {
            Locality::Local => InvestigationProviderMode::Local,
            Locality::Cloud => InvestigationProviderMode::Cloud,
        };
        request.provider_mode = mode;
        let provider_descriptor = descriptor(provider, mode).unwrap();
        let start = if let Some((active, timeout)) = configured {
            jobs.start_configured_next_prompt_investigation(
                &request,
                Some(&provider_descriptor),
                active,
                timeout,
            )
        } else {
            jobs.start_next_prompt_investigation(&request, Some(&provider_descriptor))
        }
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
    fn gated(provider: &dyn LlmProvider) -> Self {
        Self::new(provider, SpecialistId::DomainAnalystV17).into_gated()
    }
    fn gated_from_completion_base(
        provider: &dyn LlmProvider,
        base: CompletionFixture,
        request: StagedObligationRequest,
    ) -> Self {
        Self::from_completion_base(provider, SpecialistId::DomainAnalystV17, base, request)
            .into_gated()
    }
    fn into_gated(mut self) -> Self {
        let f = &mut self;
        f.request.specialist_id = "domain-analyst@18".into();
        let hash = f
            .request
            .intent_hash_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap();
        let connection = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
        let bytes: Vec<u8> = connection
            .query_row(
                "SELECT record FROM investigation_tasks WHERE id=?1",
                [&f.id],
                |row| row.get(0),
            )
            .unwrap();
        let task: Value = serde_json::from_slice(&bytes).unwrap();
        // Preserve the typed canonical field order while replacing only the cold
        // family/definition and whole-request hash; no invocation exists yet.
        let text = String::from_utf8(bytes)
            .unwrap()
            .replace(
                &serde_json::to_string(&SpecialistId::DomainAnalystV17.definition().unwrap())
                    .unwrap(),
                &serde_json::to_string(&SpecialistId::DomainAnalystV18.definition().unwrap())
                    .unwrap(),
            )
            .replace("\"domain-analyst@17\"", "\"domain-analyst@18\"")
            .replace(task["intent_hash"].as_str().unwrap(), &hash);
        let bytes = text.into_bytes();
        connection
            .execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                rusqlite::params![bytes, core_prov::content_hash(&bytes), f.id],
            )
            .unwrap();
        drop(connection);
        self
    }
    // Real inner loop only; production start/run/resume gates remain intact.
    fn run_gated_inner(&self, provider: &dyn LlmProvider) {
        self.run_gated_inner_with_clock(provider, RunClock::new(Instant::now()));
    }
    fn run_gated_inner_frozen_active(&self, provider: &dyn LlmProvider) {
        self.run_gated_inner_with_clock(provider, RunClock::frozen_active(Instant::now()));
    }
    fn run_gated_inner_with_clock(&self, provider: &dyn LlmProvider, clock: RunClock) {
        self.run_with_audit_probe(provider, clock, None);
    }
    fn run_with_audit_probe(
        &self,
        provider: &dyn LlmProvider,
        clock: RunClock,
        audit_probe: Option<&AuditProbe<'_>>,
    ) {
        self.run_with_audit_options(provider, clock, audit_probe, false);
    }
    fn run_with_audit_options(
        &self,
        provider: &dyn LlmProvider,
        clock: RunClock,
        audit_probe: Option<&AuditProbe<'_>>,
        dispatch_audit: bool,
    ) {
        self.base.base.app.run_gated_completion_inner(
            &self.id,
            &self.request,
            &self.execution,
            &self.live,
            provider,
            clock,
            audit_probe,
            dispatch_audit,
        );
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum GatedFlow {
    PartialWithholding,
    AllFindingsWithheld,
    TypedCorrection,
    ProseCorrection,
    ExhaustedProseCorrection,
    RepairReplay,
    RepeatReplay,
    RepeatDeniedEvidence,
    SpentReplay,
    ReceiptCorrection,
    OfferedTargets,
    RetainedSourceFinish,
    ParaphrasedSourceFinish,
    Repair,
    Repeat,
    Cancel,
}
struct NextProvider {
    expected_timeout_ms: u64,
    capacity_control: bool,
    observed: Scripted,
    malformed: bool,
    invalid_source_first: bool,
    metadata_order: Option<[bool; 2]>,
    metadata_query: Option<Value>,
    repeat_metadata_query: bool,
    /// Each repeat asks one more fact, so no two queries are identical.
    vary_repeated_query: bool,
    duplicate_metadata_recovery: bool,
    duplicate_inventory: bool,
    replay_metadata_query: bool,
    ignore_duplicate_finish: bool,
    query_refusal_recovery: bool,
    /// Call 2 resends the exact query answered at call 1.
    repeat_after_inventory: bool,
    received_payloads: std::sync::Mutex<Vec<llm::CompletionPayload>>,
    inventory_max_bytes: usize,
    lose_metadata_ack: bool,
    source_scenario: Option<SourceScenario>,
    extra_inventory: bool,
    large_finish: bool,
    exact_targets: bool,
    auditor_finish: bool,
    captured_withheld_finish: bool,
    gated_flow: Option<GatedFlow>,
    audit_verdict: Option<&'static str>,
    audit_cancel: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    deadlines: std::sync::Mutex<Vec<Instant>>,
}
impl NextProvider {
    fn new(malformed: bool) -> Self {
        Self {
            expected_timeout_ms: 180_000,
            capacity_control: false,
            observed: Scripted::new(Mode::CompletionNoAttempt),
            malformed,
            invalid_source_first: false,
            metadata_order: None,
            metadata_query: None,
            repeat_metadata_query: false,
            vary_repeated_query: false,
            duplicate_metadata_recovery: false,
            duplicate_inventory: false,
            replay_metadata_query: false,
            ignore_duplicate_finish: false,
            query_refusal_recovery: false,
            repeat_after_inventory: false,
            received_payloads: std::sync::Mutex::new(Vec::new()),
            inventory_max_bytes: 32768,
            lose_metadata_ack: false,
            source_scenario: None,
            extra_inventory: false,
            large_finish: false,
            exact_targets: false,
            auditor_finish: false,
            captured_withheld_finish: false,
            gated_flow: None,
            audit_verdict: None,
            audit_cancel: std::sync::Mutex::new(None),
            deadlines: std::sync::Mutex::new(Vec::new()),
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
    fn supports_bounded_response_schema(&self) -> bool {
        true
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
        assert_eq!(
            request.limits().values().request_timeout_ms,
            self.expected_timeout_ms
        );
        if self.metadata_query.is_some() || self.gated_flow.is_some() {
            self.received_payloads
                .lock()
                .unwrap()
                .push(request.payload().clone());
        }
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        self.deadlines.lock().unwrap().push(control.deadline);
        if prompt["role"] == "candidate-evidence-auditor@3" {
            let verdict = self.audit_verdict.expect("unexpected audit dispatch");
            assert_eq!(self.observed.calls.fetch_add(1, Ordering::SeqCst), 4);
            if matches!(verdict, "provider_failure" | "audit_timeout") {
                return Err(BoundedCallError {
                    code: if verdict == "audit_timeout" {
                        llm::bounded::FailureCode::DeadlineExceeded
                    } else {
                        llm::bounded::FailureCode::Transport
                    },
                    outcome: llm::bounded::InvocationOutcome::Unknown,
                });
            }
            if verdict == "cancelled_pass" {
                self.audit_cancel.lock().unwrap().as_ref().unwrap()();
                assert_eq!((control.check)(), CallDirective::Cancelled);
            }
            let verdict = if verdict == "cancelled_pass" {
                "pass"
            } else {
                verdict
            };

            let mut reviews: Vec<_> = prompt["subjects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|subject| json!({"subject":subject["subject"],"assessment":{"verdict":verdict}}))
                .collect();
            if matches!(verdict, "duplicate_subject" | "foreign_citation") {
                for review in &mut reviews {
                    review["assessment"] = json!({"verdict":"pass"});
                }
                if verdict == "duplicate_subject" {
                    assert!(
                        reviews.len() >= 2,
                        "fixture must exercise same-length duplication"
                    );
                    reviews[1]["subject"] = reviews[0]["subject"].clone();
                } else {
                    reviews[0]["assessment"] = json!({
                        "verdict":"contradiction", "citation_id":"foreign-audit-witness-canary"
                    });
                }
            }
            let text = if verdict == "invalid" {
                "not-json".into()
            } else {
                json!({"schema_version":1,"candidate_hash":prompt["candidate_hash"],
                    "acquired_ledger_hash":prompt["acquired_ledger_hash"],
                    "contract_hash":prompt["contract_hash"],"reviews":reviews})
                .to_string()
            };
            return Ok(BoundedCompletion {
                text,
                requested_model: "scripted-model".into(),
                response_model: Some("scripted-model-observed".into()),
                provider_request_id: None,
                usage: None,
                request_bytes: 100,
                response_bytes: 100,
            });
        }

        if let Some(schema) = &request.payload().response_schema
            && self.gated_flow.is_none()
            && !self.captured_withheld_finish
            // @18 selects its schema from current acquisition choices; the
            // fixed historical action-count assertion does not apply.
            && request.payload().system != SpecialistId::DomainAnalystV18.prompt()
        {
            let parsed: Value = serde_json::from_str(schema).unwrap();
            assert_eq!(
                parsed["anyOf"].as_array().unwrap().len(),
                if prompt["turn_progress"]["admission_mode"] == "finish_only" {
                    2
                } else if (request.payload().system == SpecialistId::DomainAnalystV14.prompt()
                    || request.payload().system == SpecialistId::DomainAnalystV15.prompt()
                    || request.payload().system == SpecialistId::DomainAnalystV17.prompt())
                    && parsed["anyOf"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .all(|action| action["properties"]["type"]["const"] != "read_evidence")
                {
                    5
                } else {
                    6
                }
            );
            assert!(
                request
                    .payload()
                    .system
                    .contains("Compiled analyst response schemas:")
            );
        }
        assert_eq!(prompt["schema_version"], 9);
        self.observed.prompts.lock().unwrap().push(prompt);
        let call = self.observed.calls.fetch_add(1, Ordering::SeqCst);
        // A real elapsed interval inside each scripted call witnesses that
        // the next protected pre-call sample comes from the running clock.
        std::thread::sleep(Duration::from_millis(40));
        if call == 0 && self.lose_metadata_ack {
            crate::jobs::investigations::metadata_host_fixture::lose_next_ack();
        }
        let text = if self.captured_withheld_finish {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            if call == 2 {
                let target = &prompt["completion_contract"]["obligations"]["obligations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| o["kind"] == "inventory")
                    .unwrap()["target"];
                json!({"type":"read_coverage_inventory","context_id":target["context_id"],
                    "repo":target["repo"],"inventory_hash":target["inventory_hash"],"max_bytes":32768}).to_string()
            } else {
                typed_correction_tests::partial_action(
                    prompt,
                    if call > 2 { call - 1 } else { call },
                )
                .to_string()
            }
        } else if let Some(flow) = self.gated_flow {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            if matches!(
                flow,
                GatedFlow::PartialWithholding | GatedFlow::AllFindingsWithheld
            ) {
                let mut action = typed_correction_tests::partial_action(prompt, call);
                if flow == GatedFlow::AllFindingsWithheld && call >= 2 {
                    let bad = action["action"]["findings"][if call == 2 { 1 } else { 0 }].clone();
                    action["action"]["findings"] = json!([bad]);
                }
                action.to_string()
            } else if flow == GatedFlow::RepeatDeniedEvidence {
                if call < 2 {
                    prompt["turn_progress"]["acquisition_choices"]["entries"][0]["action"]
                        .to_string()
                } else {
                    if call == 3 {
                        assert_eq!(
                            prompt["completion_metadata"]["feedback"]["prose_correction"],
                            json!({"limitation_denies_acquired_evidence":{"evidence_ids":["evidence-1"]}})
                        );
                    }
                    let mut response = typed_correction_tests::next_action(prompt, call, false);
                    response["action"]["limitations"] =
                        json!([format!("{} was not read.", request.payload().spans[0].path)]);
                    response.to_string()
                }
            } else if matches!(
                flow,
                GatedFlow::RepairReplay | GatedFlow::RepeatReplay | GatedFlow::SpentReplay
            ) {
                typed_correction_tests::replay_action(prompt, call, flow).to_string()
            } else if matches!(
                flow,
                GatedFlow::ProseCorrection | GatedFlow::ExhaustedProseCorrection
            ) {
                typed_correction_tests::prose_action(
                    prompt,
                    call,
                    flow == GatedFlow::ExhaustedProseCorrection,
                )
                .to_string()
            } else if matches!(
                flow,
                GatedFlow::TypedCorrection | GatedFlow::ReceiptCorrection
            ) {
                if call == 0 {
                    // AC-0566: the real prepared worker request offers only
                    // acquisition. This scripted provider deliberately ignores
                    // grammar to keep testing host correction defense in depth.
                    let schema: Value =
                        serde_json::from_str(request.payload().response_schema.as_deref().unwrap())
                            .unwrap();
                    let branches = schema["anyOf"].as_array().unwrap();
                    assert_eq!(branches.len(), 1);
                    assert!(
                        branches[0]["enum"]
                            .as_array()
                            .is_some_and(|a| !a.is_empty())
                    );
                }
                typed_correction_tests::next_action(
                    prompt,
                    call,
                    flow == GatedFlow::ReceiptCorrection,
                )
                .to_string()
            } else if matches!(
                flow,
                GatedFlow::RetainedSourceFinish | GatedFlow::ParaphrasedSourceFinish
            ) {
                typed_correction_tests::source_finish_action(
                    &prompts,
                    call,
                    flow == GatedFlow::ParaphrasedSourceFinish,
                )
                .to_string()
            } else if flow == GatedFlow::OfferedTargets {
                let schema: Value =
                    serde_json::from_str(request.payload().response_schema.as_deref().unwrap())
                        .unwrap();
                exact_node_tests::next_offered_action(prompt, &schema).to_string()
            } else if call == 0 || flow != GatedFlow::Repair {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":null,"labels":[],
                    "max_facts":12,"max_bytes":16384,"cursor":null}})
                .to_string()
            } else if call == 1 {
                prompt["turn_progress"]["acquisition_choices"]["entries"][0]["action"].to_string()
            } else {
                json!({"schema_version":1,"outcome":"insufficient_evidence","action":{
                    "type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                    "limitations":["This mechanical inventory read does not establish source behavior."]}}).to_string()
            }
        } else if self.exact_targets {
            let prompts = self.observed.prompts.lock().unwrap();
            exact_node_tests::next_action(prompts.last().unwrap()).to_string()
        } else if self.invalid_source_first && call == 0 {
            json!({"type":"read_evidence","fact":{"kind":"node","id":"not-admitted"},
                "role":"provenance","index":0})
            .to_string()
        } else if self.capacity_control {
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
                let target = &prompt["completion_contract"]["obligations"]["obligations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|o| o["kind"] == "inventory")
                    .unwrap()["target"];
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
                // A child audits independently; copying saved parent prose is correctly refused.
                let audit = self.auditor_finish;
                let findings = (0..if self.large_finish { 12 } else { 1 }).map(|index| json!({
                    "claim_kind":"inferred_interpretation", "title":if audit { format!("Independent source check {index}") } else { format!("Observed bounded guard {index}") },
                    "statement":if audit { "Separate inspection supports only this selected guard; surrounding paths were not acquired.".into() } else if self.large_finish { "Observed source evidence. ".repeat(72) } else { "The acquired callable includes a conditional exit.".into() },
                    "citation_ids":[citation["citation_id"]],"limitations":if audit { json!(["Execution conditions were not exercised by this audit."]) } else { json!(["Runtime inputs remain unknown."]) }
                })).collect::<Vec<_>>();
                json!({"schema_version":1,"outcome":"supported","action":{
                    "type":"finish","findings":findings,
                    "knowledge_completeness":"partial","limitations":if audit { json!(["This review leaves broader behavior unresolved."]) } else { json!(["No complete business policy was established."]) }}}).to_string()
            }
        } else if self.repeat_after_inventory && call == 2 {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            json!({"type":"query_context","context_id":prompt["context_id"],
                "query":self.metadata_query.clone().unwrap()})
            .to_string()
        } else if self.query_refusal_recovery && call == 2 {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            // Select the recovery scope from the actual admitted transport
            // schema, rather than scripting knowledge unavailable to the model.
            let schema: Value =
                serde_json::from_str(request.payload().response_schema.as_deref().unwrap())
                    .unwrap();
            let scopes = schema["$defs"]["query_scope"]["anyOf"].as_array().unwrap();
            assert_eq!(scopes.len(), 1);
            let scope_type = &scopes[0]["properties"]["type"]["const"];
            assert_eq!(scope_type, "all");
            assert_eq!(schema["$defs"]["query_cursor"], json!({"type":"null"}));
            json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                "scope":{"type":scope_type},"kind":"node","labels":["BusinessRule"],
                "max_facts":1,"max_bytes":32768,"cursor":null}})
            .to_string()
        } else if self.replay_metadata_query && call == 2 {
            let schema: Value =
                serde_json::from_str(request.payload().response_schema.as_deref().unwrap())
                    .unwrap();
            let offered = schema["anyOf"]
                .as_array()
                .unwrap()
                .iter()
                .find(|branch| branch["properties"]["type"]["const"] == "replay_query")
                .expect("the admitted schema must offer an authenticated replay");
            let fields = &offered["properties"];
            if fields["query_id"]["const"].is_null() {
                // Older families offer an open replay selector; the model
                // names the reference from its own query history.
                let prompts = self.observed.prompts.lock().unwrap();
                let prompt = prompts.last().unwrap();
                let reference =
                    &prompt["compact_context"]["query_history"]["latest_query"]["reference"];
                json!({"type":"replay_query","context_id":prompt["context_id"],
                    "query_index":reference["query_index"],"query_id":reference["query_id"]})
                .to_string()
            } else {
                json!({"type":"replay_query","context_id":fields["context_id"]["const"],
                    "query_index":fields["query_index"]["const"],"query_id":fields["query_id"]["const"]}).to_string()
            }
        } else if self.duplicate_metadata_recovery
            && (call == 2 || (call > 2 && self.ignore_duplicate_finish))
        {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            if self.duplicate_inventory {
                let inventory =
                    &prompt["completion_contract"]["obligations"]["obligations"][0]["target"];
                json!({"type":"read_coverage_inventory","context_id":inventory["context_id"],
                    "repo":inventory["repo"],"inventory_hash":inventory["inventory_hash"],
                    "max_bytes":self.inventory_max_bytes})
                .to_string()
            } else {
                json!({"type":"query_context","context_id":prompt["context_id"],"query":{
                    "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
                    "max_facts":1,"max_bytes":32768,"cursor":null}})
                .to_string()
            }
        } else if self.repeat_metadata_query {
            let prompts = self.observed.prompts.lock().unwrap();
            let prompt = prompts.last().unwrap();
            let mut query = self.metadata_query.clone().unwrap_or_else(|| {
                json!({
                    "scope":{"type":"all"},"kind":null,"labels":[],
                    "max_facts":12,"max_bytes":16384,"cursor":null
                })
            });
            if self.vary_repeated_query {
                query["max_facts"] = json!(12 + call);
            }
            json!({"type":"query_context","context_id":prompt["context_id"],"query":query})
                .to_string()
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
        let text = if self.query_refusal_recovery && call == 1 {
            let action: Value = serde_json::from_str(&text).unwrap();
            format!(
                "  {{ \"type\" : \"query_context\", \"query\" : {}, \"context_id\" : {} }}  ",
                action["query"], action["context_id"]
            )
        } else {
            text
        };
        if (call == 2
            || (call == 1 && self.query_refusal_recovery)
            || (call == 0 && self.gated_flow == Some(GatedFlow::Cancel)))
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
        (SpecialistId::DomainAnalystV13, [true, false]),
        (SpecialistId::DomainAnalystV14, [true, false]),
        (SpecialistId::DomainAnalystV15, [true, false]),
    ] {
        let mut scripted = NextProvider::new(false);
        scripted.metadata_order = Some(order);
        if matches!(
            role,
            SpecialistId::DomainAnalystV12
                | SpecialistId::DomainAnalystV13
                | SpecialistId::DomainAnalystV14
                | SpecialistId::DomainAnalystV15
        ) {
            scripted.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
                "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
        }
        let provider = Arc::new(scripted);
        let f = NextFixture::new(provider.as_ref(), role);
        f.run(provider.clone());
        if role == SpecialistId::DomainAnalystV13 {
            let received = provider.received_payloads.lock().unwrap();
            assert_eq!(received.len(), 3);
            assert!(
                received
                    .iter()
                    .all(|payload| payload.response_schema.is_some())
            );
        }
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
            if matches!(
                role,
                SpecialistId::DomainAnalystV12
                    | SpecialistId::DomainAnalystV13
                    | SpecialistId::DomainAnalystV14
                    | SpecialistId::DomainAnalystV15
            ) {
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
    // These tests exercise a retired version's own behaviour (AC-1062).
    let result = crate::investigations::commands::with_retired_starts(|| {
        test(fixture.base.app.handle(), request, &fixture.base.app_data)
    });
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
                Some(
                    "Investigation limit reached: host_limit; phase=source_prepare; limit=unspecified."
                )
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
    expanded_scripted_provider_with_timeout(partial, 180_000)
}

pub(in crate::investigations) fn expanded_scripted_provider_with_timeout(
    partial: bool,
    expected_timeout_ms: u64,
) -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.expected_timeout_ms = expected_timeout_ms;
    if partial {
        provider.source_scenario = Some(SourceScenario::Acquired);
        provider.extra_inventory = true;
    }
    Arc::new(provider)
}

// AC-0577: real acquisition and two refused finishes produce a saved withheld parent.
pub(in crate::investigations) fn expanded_withheld_scripted_provider() -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.source_scenario = Some(SourceScenario::Acquired);
    provider.extra_inventory = true;
    provider.captured_withheld_finish = true;
    Arc::new(provider)
}

// Explicit opt-in; historical parent/child/large-finish scripts retain their bytes.
pub(in crate::investigations) fn expanded_auditor_scripted_provider() -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.source_scenario = Some(SourceScenario::Acquired);
    provider.extra_inventory = true;
    provider.auditor_finish = true;
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
            DecodeStage::TypedStagedFinish,
            DecodeStage::TypedCoverageRead,
            DecodeStage::TypedLegacyAction,
            DecodeStage::TypedReplay,
            DecodeStage::BareFinish,
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
            DecodeStage::SemanticRule(Rule::FinishOverall),
            DecodeStage::SemanticRule(Rule::FindingTextLimits),
            DecodeStage::SemanticRule(Rule::FindingCitations),
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
    let finish = json!({"schema_version":1,"outcome":"supported","action":{"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"Title","statement":"Statement","citation_ids":["citation"],"limitations":["Limited."]}],"knowledge_completeness":"partial","limitations":["Limited."]}});
    let finish_cases = [
        ("/action/limitations", json!([]), Rule::FinishOverall),
        (
            "/action/findings/0/title",
            json!(""),
            Rule::FindingTextLimits,
        ),
        (
            "/action/findings/0/citation_ids",
            json!([]),
            Rule::FindingCitations,
        ),
    ]
    .map(|(pointer, value, rule)| {
        let mut response = finish.clone();
        *response.pointer_mut(pointer).unwrap() = value;
        (
            decoded(
                DecodeStage::SemanticRule(rule),
                InvestigationError::InvalidAction,
            ),
            response.to_string(),
        )
    });
    let mut nested_typed = finish.clone();
    nested_typed["action"]["findings"][0]["title"] = json!(1);
    let mut missing = finish.clone();
    missing["action"]["findings"][0]
        .as_object_mut()
        .unwrap()
        .remove("claim_kind");
    let typed_cases = [
        (missing, DecodeStage::TypedStagedFinish),
        (finish["action"].clone(), DecodeStage::BareFinish),
        (json!({"type":"read_coverage_inventory","context_id":"context","repo":1,"inventory_hash":"a".repeat(64),"max_bytes":1}), DecodeStage::TypedCoverageRead),
        (json!({"type":"replay_query","context_id":"context","query_index":"0","query_id":"a".repeat(64)}), DecodeStage::TypedReplay),
    ].map(|(value, stage)| (decoded(stage, InvestigationError::InvalidAction), value.to_string()));
    for (stage, text) in [
        (
            decoded(DecodeStage::TypedStagedFinish, InvestigationError::InvalidAction),
            nested_typed.to_string(),
        ),
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
            decoded(DecodeStage::TypedLegacyAction, InvestigationError::InvalidAction),
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
    ].into_iter().chain(finish_cases).chain(typed_cases) {
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
        // AC-1403 (ADR-0076): a response rejected at action decoding is kept
        // only as its bounded redacted record; every other stage keeps nothing.
        let kept = reopened.investigation_rejected_response(&f.id).unwrap();
        if matches!(
            stage,
            InvestigationStopReason::ActionDecode(_) | InvestigationStopReason::InvalidResponse
        ) {
            assert_eq!(
                kept,
                Some(
                    agents::investigation::rejected_response::RejectedResponse::capture(
                        "step-1",
                        &text,
                        detail.terminal_diagnostic.as_ref(),
                    )
                    .unwrap()
                )
            );
            continue;
        }
        assert_eq!(kept, None);
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
    assert_eq!(messages.len(), 18);
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
        Failure::Host(HostError::StoreCapacity),
        Failure::Host(HostError::InputChanged),
        Failure::Host(HostError::SourceUnavailable),
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
    // AC-0582: operational host/store failures keep their class and gain the phase.
    for (original, error_class) in [
        (
            Failure::Host(HostError::Operational),
            agents::investigation::InvestigationOperationalClass::HostOperational,
        ),
        (
            Failure::Store(InvestigationStoreError::Invalid),
            agents::investigation::InvestigationOperationalClass::StoreInvalid,
        ),
    ] {
        let annotated = original
            .preparation_at(Query)
            .preparation_after_call(SpecialistId::DomainAnalystV12, 1);
        assert_eq!(
            format!("{annotated:?}"),
            format!(
                "{:?}",
                Failure::Stop(InvestigationStopReason::OperationalAt {
                    phase: agents::investigation::InvestigationFailurePhase::Query,
                    error_class,
                })
            )
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

    let query_limit = Failure::Host(HostError::LimitExceeded)
        .preparation_at(Query)
        .reason()
        .unwrap();
    assert_eq!(
        query_limit,
        InvestigationStopReason::HostLimitAt {
            phase: Query,
            limit: HostLimitKind::Unspecified,
        }
    );
    assert_eq!(
        query_limit.terminal_class(),
        InvestigationStopReason::LimitExceeded
    );
    let unspecified_inventory_limit = Failure::Host(HostError::LimitExceeded)
        .preparation_at(Inventory)
        .reason()
        .unwrap();
    assert_eq!(
        unspecified_inventory_limit,
        InvestigationStopReason::HostLimitAt {
            phase: Inventory,
            limit: HostLimitKind::Unspecified,
        }
    );
    let typed_query_limit = Failure::Stop(InvestigationStopReason::HostLimitAt {
        phase: Query,
        limit: HostLimitKind::CumulativeQueryAndInventoryBytes,
    })
    .preparation_at(Metadata)
    .preparation_after_call(SpecialistId::DomainAnalystV15, 1)
    .reason()
    .unwrap();
    assert_eq!(
        typed_query_limit,
        InvestigationStopReason::HostLimitAt {
            phase: Query,
            limit: HostLimitKind::CumulativeQueryAndInventoryBytes,
        }
    );
    assert_eq!(
        typed_query_limit.terminal_class(),
        InvestigationStopReason::LimitExceeded
    );
    assert_eq!(
        Failure::Host(HostError::LimitExceeded)
            .preparation_after_call(SpecialistId::DomainAnalystV4, 1)
            .reason(),
        Some(InvestigationStopReason::HostLimit)
    );
}

// AC-0455/T-0455: the real next-family query budget refusal is phase- and cause-qualified.
#[test]
fn next_prompt_host_limit_phase_and_query_budget_refusal_are_durable() {
    let mut scripted = NextProvider::new(false);
    scripted.repeat_metadata_query = true;
    scripted.vary_repeated_query = true;
    scripted.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
        "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV15);
    f.run(provider.clone());

    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some(
            "Investigation limit reached: host_limit; phase=metadata_query; limit=cumulative_query_and_inventory_bytes."
        )
    );
    assert_eq!(
        detail.usage.tool_actions as usize,
        provider.observed.calls.load(Ordering::SeqCst) - 1
    );
    let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
    assert_eq!(
        provider.observed.calls.load(Ordering::SeqCst),
        ledger.queries.len() + 1
    );
    assert!(ledger.queries.len() > 1);
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert_eq!(
        events
            .items
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::ModelCompleted)
            .count(),
        ledger.queries.len()
    );
    assert_eq!(
        events.items.last().unwrap().kind,
        InvestigationEventKind::Failed
    );
    assert_eq!(
        events.items.last().unwrap().summary,
        detail.error.clone().unwrap()
    );
    assert!(
        events
            .items
            .iter()
            .rev()
            .take(2)
            .any(|event| { event.kind == InvestigationEventKind::ModelStarted })
    );
    drop(jobs);

    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
}

// AC-1412 (#1352): an exact repeat of an answered query adds nothing. A
// model without closed action feedback stops before spending the remaining
// query bytes on the same page.
#[test]
fn next_prompt_exact_repeated_query_stops_with_repeated_query_limit() {
    let mut scripted = NextProvider::new(false);
    scripted.repeat_metadata_query = true;
    scripted.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
        "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV15);
    f.run(provider.clone());

    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        detail.error.as_deref(),
        Some(
            "Investigation limit reached: host_limit; phase=metadata_query; limit=repeated_query."
        )
    );
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
    let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 1);
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert_eq!(
        events.items.last().unwrap().summary,
        detail.error.clone().unwrap()
    );
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
}

// AC-1412 (#1352): an authenticated replay_query names an answered query on
// purpose, so @17 (no acquisition choices) runs it instead of stopping.
#[test]
fn desktop_v17_authenticated_query_replay_is_not_a_repeated_query() {
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, true]);
    scripted.replay_metadata_query = true;
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV17);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 2);
    assert_eq!(ledger.queries[1].query, ledger.queries[0].query);
}

// AC-1412 (#1352): @18 has closed action feedback, so an exact repeat is
// refused as repeated_read and the model continues without a second page.
#[test]
fn desktop_v18_exact_repeated_query_is_refused_and_recovers() {
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, true]);
    scripted.repeat_after_inventory = true;
    scripted.metadata_query = Some(json!({
        "scope":{"type":"all"},"kind":"node","labels":["BusinessRule"],
        "max_facts":1,"max_bytes":32768,"cursor":null
    }));
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert!(detail.error.is_none());
    let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 1);
    let prompts = provider.observed.prompts.lock().unwrap();
    assert_eq!(
        prompts[3]["turn_progress"]["action_feedback"]["reason"],
        "repeated_read"
    );
    drop(prompts);
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    drop(jobs);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
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
    // AC-1405 (ADR-0076): the admitted query, cursor included, is kept only as
    // the canonical action body its saved hash covers; no response excerpt is.
    assert_eq!(
        reopened.investigation_rejected_response(&f.id).unwrap(),
        None
    );
    let kept = reopened
        .investigation_admitted_action(&f.id)
        .unwrap()
        .unwrap();
    let agents::investigation::admitted_action::ActionRetention::Body { json } = &kept.retention
    else {
        panic!("expected the admitted action body");
    };
    assert!(json.contains("PRIVATE-FOREIGN-CURSOR-1293"));
    assert_eq!(kept.action_hash, core_prov::content_hash(json.as_bytes()));
}

// AC-0447: a scripted schema bypass stops semantically before captured-source preparation.
#[test]
fn next_prompt_unadmitted_source_fact_stops_before_preparation() {
    for role in [
        SpecialistId::DomainAnalystV14,
        SpecialistId::DomainAnalystV15,
    ] {
        let mut provider = NextProvider::new(false);
        provider.invalid_source_first = true;
        let provider = Arc::new(provider);
        let fixture = NextFixture::new(provider.as_ref(), role);
        fixture.run(provider.clone());
        let state = fixture.base.base.app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .unwrap()
            .investigation_owned_detail(&fixture.execution, &fixture.id)
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Failed);
        let expected = InvestigationStopReason::ActionDecode(ActionDecodeFailure {
            stage: ActionDecodeStage::SemanticRule(ActionSemanticRule::EvidenceShape),
            error: InvestigationError::InvalidAction,
        })
        .test_message();
        assert_eq!(detail.error.as_deref(), Some(expected));
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 1);
        assert_eq!(detail.usage.tool_actions, 0);
    }
}

#[path = "exact_node_tests.rs"]
mod exact_node_tests;

// AC-0552: normal run_inner accepts the exact saved family/request, but never a
// mismatched request. This intentionally does not call the private inner seam.
#[test]
fn explicit_acquisition_saved_task_runs_normal_worker() {
    for mismatched in [false, true] {
        let mut scripted = NextProvider::new(false);
        scripted.gated_flow = Some(GatedFlow::Repair);
        let provider = Arc::new(scripted);
        let mut f = NextFixture::gated(provider.as_ref());
        if mismatched {
            f.request.specialist_id = "domain-analyst@17".into();
        }
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let detail = state.jobs.lock().unwrap().investigation(&f.id).unwrap();
        assert_eq!(detail.summary.specialist_id, SpecialistId::DomainAnalystV18);
        assert_eq!(
            provider.observed.calls.load(Ordering::SeqCst),
            if mismatched { 0 } else { 3 }
        );
        assert_eq!(
            detail.usage.model_invocations,
            if mismatched { 0 } else { 3 }
        );
        assert_eq!(detail.usage.tool_actions, u32::from(!mismatched));
        assert_eq!(
            detail.summary.status,
            if mismatched {
                InvestigationStatus::Failed
            } else {
                InvestigationStatus::Completed
            },
            "{:?}",
            detail.error
        );
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
        assert_eq!(
            reopened.investigation_result(&f.id).unwrap().is_some(),
            !mismatched
        );
    }
}

#[path = "initial_acquisition_tests.rs"]
mod initial_acquisition_tests;

// AC-0266: after required acquisitions, @18 permits exploratory queries. A
// missing selector must not be confused with a corrupt host snapshot.
#[test]
fn desktop_v18_unknown_anchor_after_inventory_recovers() {
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, true]);
    scripted.query_refusal_recovery = true;
    scripted.metadata_query = Some(json!({
        "scope":{"type":"neighborhood","anchor":"absent-query-refusal-anchor","hops":1},
        "kind":null,"labels":[],"max_facts":1,"max_bytes":16384,"cursor":null
    }));
    let provider = Arc::new(scripted);
    // Genuine current-family start, not the historical gated fixture rewrite.
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
    assert_eq!(detail.usage.model_invocations, 4);
    assert_eq!(detail.usage.tool_actions, 3);
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert!(detail.error.is_none());
    let prompts = provider.observed.prompts.lock().unwrap();
    assert_eq!(
        prompts[2]["turn_progress"]["action_feedback"]["reason"],
        "unknown_anchor"
    );
    assert!(
        !prompts[2]
            .to_string()
            .contains("absent-query-refusal-anchor")
    );
    assert!(prompts[3]["turn_progress"]["action_feedback"].is_null());
    drop(prompts);
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert!(
        !events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ActionRefused)
    );
    assert_eq!(
        events
            .items
            .iter()
            .filter(|event| event.kind == InvestigationEventKind::ToolCompleted)
            .count(),
        2
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
}

// AC-0266, AC-0187: cancellation of the in-flight query cannot publish
// refusal feedback or authorize the next invocation.
#[test]
fn desktop_v18_cancelled_query_does_not_publish_refusal_or_retry() {
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, true]);
    scripted.query_refusal_recovery = true;
    scripted.metadata_query = Some(json!({
        "scope":{"type":"neighborhood","anchor":"absent-query-refusal-anchor","hops":1},
        "kind":null,"labels":[],"max_facts":1,"max_bytes":16384,"cursor":null
    }));
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18);
    let app = f.base.base.app.handle().clone();
    let id = f.id.clone();
    *provider.observed.cancel.lock().unwrap() = Some(Box::new(move || {
        let state = app.state::<AppState>();
        let mut jobs = state.jobs.lock().unwrap();
        let before = jobs.investigation(&id).unwrap();
        assert_eq!(before.usage.model_invocations, 2);
        assert_eq!(before.usage.tool_actions, 1);
        jobs.cancel_next_prompt_investigation(&id).unwrap();
    }));
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
    assert_eq!(detail.usage.model_invocations, 2);
    assert_eq!(detail.usage.tool_actions, 1);
    assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
    assert!(jobs.investigation_result(&f.id).unwrap().is_none());
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert!(
        !events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::QueryRefused)
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);
}

#[path = "next_prompt_timeout_witness.rs"]
mod timeout_witness;

// AC-0260: ordinary dispatch charges one duplicate notice, then finishes/reopens.
#[test]
fn desktop_v18_duplicate_read_enters_authenticated_finish_only() {
    for (inventory, ignore_finish, configured) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (false, false, true),
        (true, false, true),
    ] {
        let mut scripted = NextProvider::new(false);
        if configured {
            scripted.expected_timeout_ms = 600_000;
        }
        scripted.metadata_order = Some([false, true]);
        scripted.duplicate_metadata_recovery = true;
        scripted.duplicate_inventory = inventory;
        scripted.ignore_duplicate_finish = ignore_finish;
        let provider = Arc::new(scripted);
        let f = if configured {
            let base = CompletionFixture::new(
                provider.as_ref(),
                SpecialistId::DomainAnalystV9,
                false,
                true,
                false,
            );
            let request = base.request.clone();
            NextFixture::from_completion_base_with_limits(
                provider.as_ref(),
                SpecialistId::DomainAnalystV18,
                base,
                request,
                Some((3600, 600_000)),
            )
        } else {
            NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18)
        };
        f.run(provider.clone());
        let state = f.base.base.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&f.id).unwrap();
        if configured {
            let (owned, timeout) = jobs
                .investigation_owned_runtime(&f.execution, &f.id)
                .unwrap();
            assert_eq!(owned, detail);
            assert_eq!(owned.limits.active_seconds, 3600);
            assert_eq!(timeout, 600_000);
        }
        assert_eq!(
            detail.summary.status,
            if ignore_finish {
                InvestigationStatus::Failed
            } else {
                InvestigationStatus::Completed
            },
            "{:?}",
            detail.error
        );
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 4);
        assert_eq!(detail.usage.tool_actions, 3);
        if ignore_finish {
            assert!(jobs.investigation_result(&f.id).unwrap().is_none());
            assert!(
                detail.error.as_deref()
                    == Some(
                        "The provider proposed a tool when only a finishing response was permitted."
                    ),
                "{:?}",
                detail.error
            );
        }
        let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
        assert_eq!(ledger.queries.len(), 1);
        assert_eq!(ledger.coverage_reads.len(), 1);
        let prompts = provider.observed.prompts.lock().unwrap();
        assert_eq!(prompts[3]["turn_progress"]["admission_mode"], "finish_only");
        assert_eq!(
            prompts[3]["turn_progress"]["action_feedback"]["reason"],
            "repeated_read"
        );
        let events = jobs.investigation_events(&f.id, 0).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == InvestigationEventKind::ToolCompleted)
                .count(),
            2
        );
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == InvestigationEventKind::ActionRefused)
                .count(),
            1
        );
        let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
        assert_eq!(reopened.investigation_events(&f.id, 0).unwrap(), events);

        // Rehashing an invocation after removing the finish witness must not
        // erase the journal's reason for restricting this exact saved call.
        if !ignore_finish {
            let conn = rusqlite::Connection::open(f.base.base.app_data.join("state.db")).unwrap();
            let rows = conn
                .prepare("SELECT step_id,body FROM investigation_invocations WHERE task_id=?1")
                .unwrap()
                .query_map([&f.id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let (step, original) = rows
                .into_iter()
                .find(|(_, bytes)| {
                    let value: Value = serde_json::from_slice(bytes).unwrap();
                    value["disclosure"]["admission"]["acquisition"]["finish_reason"]
                        == "repeated_read"
                })
                .unwrap();
            let mut forged: Value = serde_json::from_slice(&original).unwrap();
            forged["disclosure"]["admission"]["acquisition"]
                .as_object_mut()
                .unwrap()
                .remove("finish_reason");
            let forged = serde_json::to_vec(&forged).unwrap();
            let trigger: String = conn.query_row("SELECT sql FROM sqlite_schema WHERE name='investigation_invocations_no_update'", [], |row| row.get(0)).unwrap();
            for (bytes, valid) in [(&forged, false), (&original, true)] {
                let tx = conn.unchecked_transaction().unwrap();
                tx.execute_batch("DROP TRIGGER investigation_invocations_no_update")
                    .unwrap();
                tx.execute("UPDATE investigation_invocations SET body=?1,body_hash=?2 WHERE task_id=?3 AND step_id=?4",
                    rusqlite::params![bytes,content_hash(bytes),f.id,step]).unwrap();
                tx.execute_batch(&trigger).unwrap();
                tx.commit().unwrap();
                assert_eq!(reopened.investigation(&f.id).is_ok(), valid);
            }
        }
    }
}

// AC-0260: an offered authenticated replay is intentional, not a duplicate refusal.
#[test]
fn desktop_v18_authenticated_query_replay_executes_and_reopens() {
    let mut scripted = NextProvider::new(false);
    scripted.metadata_order = Some([false, true]);
    scripted.replay_metadata_query = true;
    let provider = Arc::new(scripted);
    let f = NextFixture::new(provider.as_ref(), SpecialistId::DomainAnalystV18);
    f.run(provider.clone());
    let state = f.base.base.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&f.id).unwrap();
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert_eq!(detail.usage.model_invocations, 4);
    assert_eq!(detail.usage.tool_actions, 3);
    let ledger = jobs.investigation_ledger(&f.id).unwrap().unwrap();
    assert_eq!(ledger.queries.len(), 2);
    assert_eq!(ledger.coverage_reads.len(), 1);
    let events = jobs.investigation_events(&f.id, 0).unwrap();
    assert_eq!(
        events
            .items
            .iter()
            .filter(|e| e.kind == InvestigationEventKind::ToolCompleted)
            .count(),
        3
    );
    assert_eq!(
        events
            .items
            .iter()
            .filter(|e| e.kind == InvestigationEventKind::ActionRefused)
            .count(),
        0
    );
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&f.id).unwrap(), detail);
    assert_eq!(reopened.investigation_ledger(&f.id).unwrap(), Some(ledger));
}

// AC-0188, AC-0562: genuine admission freezes both time limits across
// query/inventory continuations, a settings change, finish and durable reopen.
#[test]
fn configured_desktop_worker_preserves_time_limits_across_calls_and_reopen() {
    configured_desktop_worker_time_limits(false, 0);
}

// AC-0187, AC-0562: a valid late finish is retained without another tool action.
#[test]
fn configured_desktop_worker_late_cancel_retains_valid_finish_and_time_limits() {
    configured_desktop_worker_time_limits(true, 0);
}

// AC-0188, AC-0562: the real worker clock must honor the saved active limit.
#[test]
fn configured_desktop_worker_continues_after_default_active_deadline() {
    configured_desktop_worker_time_limits(false, 601);
}

fn configured_desktop_worker_time_limits(cancel_finish: bool, elapsed_seconds: u64) {
    let mut scripted = NextProvider::new(false);
    scripted.expected_timeout_ms = 600_000;
    // The current family requires inventory before exploratory queries.
    scripted.metadata_order = Some([false, true]);
    scripted.metadata_query = Some(json!({"scope":{"type":"all"},"kind":null,
        "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}));
    let provider = Arc::new(scripted);
    let base = CompletionFixture::new(
        provider.as_ref(),
        SpecialistId::DomainAnalystV9,
        false,
        true,
        false,
    );
    let request = base.request.clone();
    let f = NextFixture::from_completion_base_with_limits(
        provider.as_ref(),
        SpecialistId::DomainAnalystV18,
        base,
        request,
        Some((3600, 600_000)),
    );
    f.base
        .base
        .app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap()
        .set_run_limits(600, 180_000)
        .unwrap();
    if cancel_finish {
        let app = f.base.base.app.handle().clone();
        let id = f.id.clone();
        *provider.observed.cancel.lock().unwrap() = Some(Box::new(move || {
            app.state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .cancel_next_prompt_investigation(&id)
                .unwrap();
        }));
    }
    f.base.base.app.run_completion(
        f.id.clone(),
        f.request.clone(),
        f.execution.clone(),
        provider.clone(),
        f.live.clone(),
        Instant::now() - Duration::from_secs(elapsed_seconds),
    );
    let state = f.base.base.app.state::<AppState>();
    let (detail, timeout) = state
        .jobs
        .lock()
        .unwrap()
        .investigation_owned_runtime(&f.execution, &f.id)
        .unwrap();
    assert_eq!(timeout, 600_000);
    assert_eq!(detail.limits.active_seconds, 3600);
    assert_eq!(
        detail.summary.status,
        InvestigationStatus::Completed,
        "{:?}",
        detail.error
    );
    assert!(detail.summary.has_result);
    assert_eq!(detail.summary.cancel_requested, cancel_finish);
    assert_eq!(detail.usage.model_invocations, 3);
    assert_eq!(detail.usage.tool_actions, 2);
    assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 3);
    let reopened = crate::jobs::JobStore::open(f.base.base.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened
            .investigation_owned_runtime(&f.execution, &f.id)
            .unwrap(),
        (detail, timeout)
    );
}

#[path = "typed_correction_tests.rs"]
mod typed_correction_tests;

#[path = "semantic_audit_start_tests.rs"]
mod semantic_audit_start_tests;

#[test]
fn inline_completion_fixture_releases_stores_and_preserves_handle_escape() {
    // AC-0287: lifecycle follows the real worker, including ordinary failure.
    for escape in [false, true] {
        let provider = NextProvider::new(false);
        let fixture = NextFixture::new(&provider, SpecialistId::DomainAnalystV10);
        let weak = Arc::downgrade(&fixture.base.base.app.state::<AppState>().sources);
        let escaped = escape.then(|| fixture.base.base.app.handle().clone());
        fixture.run_gated_inner(&provider);
        assert_eq!(
            fixture
                .base
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
            InvestigationStatus::Failed
        );
        assert_eq!(provider.observed.calls.load(Ordering::SeqCst), 2);
        drop(fixture);
        assert_eq!(weak.upgrade().is_some(), escape);
        if let Some(handle) = escaped {
            assert!(handle.try_state::<AppState>().is_some());
        }
    }
}

#[path = "hosted_source_limits_tests.rs"]
mod hosted_source_limits_tests;

pub(in crate::investigations) use history_tests::full_hosted_auditor_provider;
pub(in crate::investigations) fn expanded_large_scripted_provider() -> Arc<dyn LlmProvider> {
    let mut provider = NextProvider::new(false);
    provider.source_scenario = Some(SourceScenario::Acquired);
    provider.extra_inventory = true;
    provider.large_finish = true;
    Arc::new(provider)
}

#[path = "auditor12_budget_tests.rs"]
mod auditor12_budget_tests;

#[path = "auditor14_capacity_tests.rs"]
mod auditor14_capacity_tests;
