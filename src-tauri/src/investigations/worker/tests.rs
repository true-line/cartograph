use super::*;
use adapters_lang_ts::captured;
use core_graph::GraphStore;
use core_prov::{ConfidenceTier, Tier};
use llm::bounded::{
    AuthorizedBoundedCompletion, FailureCode, InvocationOutcome, ProviderProfile, ReportedUsage,
};
use llm::{Embedding, ProviderCaps, ProviderError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const ORIGINAL: &str = "// private trailing-source marker is never output\nexport function enabled(ok: boolean) { const limited = ok === false; if (limited) return false; }\n";

#[derive(Clone, Copy)]
enum Mode {
    Finish,
    CancelFinish,
    CancelTool,
    Malformed,
    Unknown,
    ResponseTooLarge,
    UnknownResponseTooLarge,
    CompletionHostLimit,
    CompletionUnknownAnchor,
    ForeignCursor,
    RepeatQuery,
    RepeatRead,
    UnavailableRead,
    PanicAfterQuery,
    MetadataFinish,
    MetadataReplay,
    MetadataReplayCorrection,
    MetadataReplayCancelled,
    PriorFindingReplay,
    PriorFindingCorrection,
    PriorFindingCorrectionToolProposal,
    PriorFindingCorrectionProviderFailure,
    UnadmittedRead,
    InventoryFinish,
    WrongQualifiedContext,
    CompletionSupported,
    SpecWriterSupported,
    SpecWriterMessage,
    SpecWriterMessageInsufficient,
    GapTriagerSupported,
    GapTriagerMessage,
    CompletionRepair,
    CompletionCap,
    CompletionInventoryUnavailable,
    CompletionNoAttempt,
    CompletionRedacted,
    CompletionCancelRefused,
    ProgressiveReplay,
    ProgressiveReplayWrongContext,
    ProgressiveReplayWrongIndex,
    ProgressiveReplayWrongId,
}

/// One deterministic provider action per actual host invocation. The fixture
/// learns exact fact/citation/range IDs only from its admitted prompt; no
/// independent expected answer is placed in that prompt or target source.
struct Scripted {
    calls: AtomicUsize,
    mode: Mode,
    cancel: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    copied: Mutex<Vec<String>>,
    prompts: Mutex<Vec<Value>>,
    schema: bool,
}
impl Scripted {
    fn new(mode: Mode) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mode,
            cancel: Mutex::new(None),
            copied: Mutex::new(vec![]),
            prompts: Mutex::new(vec![]),
            schema: true,
        }
    }
    /// A provider that cannot carry a bounded response schema.
    fn without_schema(mode: Mode) -> Self {
        Self {
            schema: false,
            ..Self::new(mode)
        }
    }
}
impl LlmProvider for Scripted {
    fn id(&self) -> &str {
        "local-scripted"
    }
    /// Like the Ollama endpoint it stands in for (#1989).
    fn supports_bounded_response_schema(&self) -> bool {
        self.schema
    }
    fn locality(&self) -> Locality {
        Locality::Local
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            embeddings: false,
            chat: true,
            tool_use: false,
        }
    }
    fn embed(&self, _: &[String]) -> Result<Vec<Embedding>, ProviderError> {
        Err(ProviderError::Unsupported("fixture embeddings"))
    }
    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        Ok(ProviderProfile {
            protocol_version: llm::bounded::PROTOCOL_VERSION.into(),
            provider_id: self.id().into(),
            locality: Locality::Local,
            endpoint_id: "http://127.0.0.1:11434/api/chat".into(),
            requested_model: "scripted-model".into(),
        })
    }
    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        assert_eq!((control.check)(), CallDirective::Continue);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if matches!(
            self.mode,
            Mode::ResponseTooLarge | Mode::UnknownResponseTooLarge
        ) {
            return Err(BoundedCallError {
                code: FailureCode::ResponseTooLarge,
                outcome: if matches!(self.mode, Mode::UnknownResponseTooLarge) {
                    InvocationOutcome::Unknown
                } else {
                    InvocationOutcome::ResponseReceived
                },
            });
        }
        if matches!(self.mode, Mode::Unknown) {
            return Err(BoundedCallError {
                code: FailureCode::Transport,
                outcome: InvocationOutcome::Unknown,
            });
        }
        let prompt: Value = serde_json::from_str(&request.payload().prompt).unwrap();
        self.prompts.lock().unwrap().push(prompt.clone());
        if call == 3 && matches!(self.mode, Mode::PriorFindingCorrectionProviderFailure) {
            return Err(BoundedCallError {
                code: FailureCode::Transport,
                outcome: InvocationOutcome::NotDispatched,
            });
        }
        assert!(
            !(call == 1 && matches!(self.mode, Mode::PanicAfterQuery)),
            "simulated owner loss"
        );
        let mut action = if matches!(self.mode, Mode::ForeignCursor) {
            json!({"type":"query_context","query":{"scope":{"type":"all"},"kind":null,
                "labels":[],"max_facts":12,"max_bytes":16384,"cursor":{
                    "snapshot_id":"PRIVATE-FOREIGN-CURSOR-1293","selection_id":"foreign-selection","offset":0}}})
        } else if matches!(prompt["schema_version"].as_u64(), Some(6..=8)) {
            completion_tests::scripted_action(self.mode, call, &prompt)
        } else if matches!(self.mode, Mode::UnadmittedRead) {
            assert_eq!(call, 0, "unadmitted reads never buy a second call");
            json!({"type":"read_evidence", "fact":{"kind":"node", "id":"node:unadmitted"},
                "role":"provenance", "index":0})
        } else if matches!(self.mode, Mode::InventoryFinish) {
            if call == 0 {
                let repo = &prompt["coverage_qualification"]["repos"][0];
                json!({"type":"read_coverage_inventory","context_id":prompt["context_id"],
                    "repo":repo["repo"],"inventory_hash":repo["inventory_hash"],"max_bytes":32768})
            } else {
                assert_eq!(call, 1, "inventory finish has no hidden extra invocation");
                assert_eq!(prompt["coverage_details"].as_array().unwrap().len(), 1);
                json!({"type":"finish","findings":[],"knowledge_completeness":"insufficient_evidence",
                    "limitations":["Coverage records cannot establish source behavior."]})
            }
        } else if call == 0 || matches!(self.mode, Mode::RepeatQuery) {
            if matches!(self.mode, Mode::RepeatQuery) {
                json!({ "type":"query_context", "query": { "scope":{"type":"all"}, "kind":null,
                    "labels":[], "max_facts":12, "max_bytes":16384, "cursor":null } })
            } else {
                json!({ "type": "query_context", "query": { "scope": {"type":"all"}, "kind":"node",
                    "labels":if matches!(self.mode, Mode::MetadataReplay | Mode::MetadataReplayCorrection | Mode::MetadataReplayCancelled) { json!(["Gap"]) } else { json!(["BusinessRule"]) }, "max_facts":1, "max_bytes":32768, "cursor":null } })
            }
        } else if (call == 1
            && matches!(
                self.mode,
                Mode::MetadataFinish
                    | Mode::MetadataReplay
                    | Mode::MetadataReplayCorrection
                    | Mode::MetadataReplayCancelled
            ))
            || (call == 2 && matches!(self.mode, Mode::MetadataReplay))
        {
            let citations = prompt
                .get("citations")
                .unwrap_or(&prompt["input_ledger"]["citations"])
                .as_array()
                .unwrap();
            json!({"type":"finish","findings":[{"claim_kind":"inferred_interpretation","title":"Scoped graph observation",
                "statement":"The recovered context includes a guarded path; source remains uninspected.","citation_ids":[citations[0]["citation_id"]],
                "limitations":["No source bytes were acquired."]}],"knowledge_completeness":"partial","limitations":["Graph metadata alone was inspected."]})
        } else if call == 1 || (call == 2 && matches!(self.mode, Mode::RepeatRead)) {
            let option = if prompt.get("compact_context").is_some() {
                &prompt["compact_context"]["evidence_options"][0]
            } else {
                &prompt["context_pages"][0]["evidence_options"][0]
            };
            let range = option["ranges"]
                .as_array()
                .unwrap()
                .iter()
                .find(|range| range["role"] == "definition_initializer")
                .unwrap();
            json!({ "type":"read_evidence", "fact":option["fact"], "role":range["role"],
                "index":if matches!(self.mode, Mode::UnavailableRead) { json!(999) } else { range["index"].clone() } })
        } else if call == 2
            && matches!(
                self.mode,
                Mode::PriorFindingReplay
                    | Mode::PriorFindingCorrection
                    | Mode::PriorFindingCorrectionToolProposal
                    | Mode::PriorFindingCorrectionProviderFailure
            )
            || (call == 3 && matches!(self.mode, Mode::PriorFindingReplay))
        {
            let raw = prompt["prior_findings_as_t3_history"][0].as_str().unwrap();
            let prior: Value = serde_json::from_str(raw).unwrap();
            let citation = prompt
                .get("citations")
                .unwrap_or(&prompt["input_ledger"]["citations"])
                .as_array()
                .unwrap()
                .iter()
                .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            json!({"type":"finish","findings":[{"claim_kind":"inferred_interpretation",
                "title":"A guarded path was observed",
                "statement":prior["saved_result"]["findings"][0]["statement"],
                "citation_ids":[citation["citation_id"]],
                "limitations":["Only the selected captured range was inspected."]}],
                "knowledge_completeness":"partial","limitations":["The source range does not establish all runtime behavior."]})
        } else if call == 2 && matches!(self.mode, Mode::MetadataReplayCorrection) {
            let citation = prompt
                .get("citations")
                .unwrap_or(&prompt["input_ledger"]["citations"])
                .as_array()
                .unwrap()
                .first()
                .unwrap();
            json!({"type":"finish","findings":[{"claim_kind":"inferred_interpretation",
                "title":"A bounded graph fact was observed",
                "statement":"The recorded graph metadata supports this limited observation.",
                "citation_ids":[citation["citation_id"]],
                "limitations":["This synthetic graph observation does not verify source behavior."]}],
                "knowledge_completeness":"partial","limitations":["Only admitted graph metadata was considered."]})
        } else if call == 3 && matches!(self.mode, Mode::PriorFindingCorrectionToolProposal) {
            json!({"type":"query_context","query":{"scope":{"type":"all"},"kind":null,
                "labels":[],"max_facts":12,"max_bytes":16384,"cursor":null}})
        } else if call == 3 && matches!(self.mode, Mode::PriorFindingCorrection) {
            assert_eq!(prompt["finish_correction"]["refusal"], "source_replay");
            let citation = prompt
                .get("citations")
                .unwrap_or(&prompt["input_ledger"]["citations"])
                .as_array()
                .unwrap()
                .iter()
                .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            json!({"type":"finish","findings":[{"claim_kind":"inferred_interpretation",
                "title":"A conditional return is present",
                "statement":"The selected function returns false when its explicit Boolean condition is false.",
                "citation_ids":[citation["citation_id"]],
                "limitations":["This selected range does not establish other runtime paths."]}],
                "knowledge_completeness":"partial","limitations":["Only the selected source range was inspected."]})
        } else {
            assert_eq!(
                call,
                if matches!(self.mode, Mode::RepeatRead) {
                    3
                } else {
                    2
                },
                "no hidden repair, retry or extra model step"
            );
            let citations = prompt
                .get("citations")
                .unwrap_or(&prompt["input_ledger"]["citations"])
                .as_array()
                .unwrap();
            let citation = citations
                .iter()
                .find(|citation| citation["origin"]["kind"] == "captured_primary_source")
                .unwrap();
            let span = request
                .payload()
                .spans
                .iter()
                .find(|span| span.id == citation["citation_id"])
                .unwrap();
            self.copied.lock().unwrap().push(span.text.clone());
            json!({ "type":"finish", "findings":[{ "claim_kind":"inferred_interpretation",
                "title":"A guarded path was observed", "statement":"The inspected callable can exit conditionally; external policy remains unresolved.",
                "citation_ids":[citation["citation_id"]], "limitations":["Runtime inputs and broader feature coverage remain unknown."] }],
                "knowledge_completeness":"partial", "limitations":["This scoped result establishes no complete business policy."] })
        };
        if (matches!(self.mode, Mode::MetadataReplay)
            || (matches!(
                self.mode,
                Mode::MetadataReplayCorrection | Mode::MetadataReplayCancelled
            ) && call == 1))
            && action["type"] == "finish"
        {
            action["findings"][0]["statement"] = json!(refusal_diagnostics::METADATA);
        }
        if action["type"] == "query_context" && !prompt["context_id"].is_null() {
            action["context_id"] = if matches!(self.mode, Mode::WrongQualifiedContext) {
                json!(format!("context-v2:{}", "f".repeat(64)))
            } else {
                prompt["context_id"].clone()
            };
        }
        if (matches!(self.mode, Mode::MetadataReplayCancelled) && call == 1)
            || (matches!(self.mode, Mode::SpecWriterMessage)
                && call == 3
                && self.cancel.lock().unwrap().is_some())
            || (matches!(self.mode, Mode::CancelFinish) && call == 2)
            || (matches!(self.mode, Mode::CancelTool | Mode::CompletionCancelRefused) && call == 1)
        {
            self.cancel.lock().unwrap().as_ref().unwrap()();
            // The response was already in flight: emulate its valid return
            // after cancellation instead of fabricating remote termination.
        }
        let text = if matches!(self.mode, Mode::Malformed) {
            "```json\n{}\n```".into()
        } else if matches!(self.mode, Mode::CompletionUnknownAnchor) && call == 0 {
            format!(
                "  {{ \"type\" : \"query_context\", \"query\" : {}, \"context_id\" : {} }}  ",
                action["query"], action["context_id"]
            )
        } else {
            serde_json::to_string(&action).unwrap()
        };
        Ok(BoundedCompletion {
            text,
            requested_model: "scripted-model".into(),
            response_model: Some("scripted-model-observed".into()),
            provider_request_id: None,
            usage: Some(ReportedUsage {
                input_tokens: Some(100),
                output_tokens: Some(40),
                cache_read_input_tokens: Some(7),
                cache_creation_input_tokens: None,
            }),
            request_bytes: 100,
            response_bytes: 100,
        })
    }
}

// Shared capture-only setup: preparing a control must not create a legacy task.
fn captured_fixture_state(
    directory: &std::path::Path,
    original: &str,
    qualified: bool,
    inventory_json: Option<fn(&str) -> String>,
) -> (AppState, PathBuf, String) {
    let app_data = directory.join("private");
    std::fs::create_dir(&app_data).unwrap();
    let app_data = crate::paths::canonicalize(app_data).unwrap();
    let target = directory.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("rule.ts"), original).unwrap();
    let state = crate::registered_source_tests::app_state(&app_data);
    let source = state
        .sources
        .lock()
        .unwrap()
        .register_local(&target)
        .unwrap();
    let input = state
        .primary_sources
        .prepare(&source, source.root(), &["client".into()])
        .unwrap();
    let capture = input.capture.as_ref().unwrap();
    let (extraction, receipts) = captured::extract_file(
        capture.file("rule.ts").unwrap(),
        &adapters_lang_ts::SourceId {
            repo: &source.repo_key,
            commit: "workdir",
        },
    )
    .unwrap();
    state
        .primary_sources
        .persist(&input, &source, &receipts)
        .unwrap();
    let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
    assert!(!bindings.is_empty());
    let coverage = qualified.then(|| core_graph::coverage::CoverageRecord {
        repo_key: source.repo_key.clone(),
        registered_source_id: Some(source.source_id.clone()),
        ingest_id: "worker-fixture-ingest".into(),
        config_hash: crate::coverage_config_hash(&[], crate::Registries::builtin()),
        inventory_json: inventory_json.map_or_else(
            || {
                json!({"inventory":"coverage@1","repo":source.repo_key,
                    "commit":"workdir","layer_hints":[],"producers":[{"state":"missing_input"}],
                    "unsupported":[],"limited":true})
                .to_string()
            },
            |inventory| inventory(&source.repo_key),
        ),
    });
    crate::load_into_graph_with_bindings(
        &mut state.graph.lock().unwrap(),
        &extraction,
        &source.repo_key,
        source.root(),
        "workdir",
        &bindings,
        coverage.as_ref(),
    )
    .unwrap();
    drop(input);
    // Every later read must use retained parser bytes, not this checkout.
    std::fs::remove_dir_all(target).unwrap();
    let graph_before =
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap();
    (state, app_data, graph_before)
}

#[path = "fixture_app.rs"]
mod fixture_app;
use fixture_app::FixtureApp;

struct Fixture {
    app: FixtureApp,
    _directory: tempfile::TempDir,
    app_data: PathBuf,
    request: StartInvestigationRequest,
    id: String,
    execution: JobExecution,
    live: Arc<LiveTask>,
    graph_before: String,
    start_milliseconds: u128,
}

impl Fixture {
    fn new(provider: &dyn LlmProvider) -> Self {
        Self::with_question(
            provider,
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
        )
    }

    fn with_question(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
    ) -> Self {
        Self::with_specialist(provider, directory, question, SpecialistId::DomainAnalyst)
    }

    fn with_specialist(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
        specialist_id: SpecialistId,
    ) -> Self {
        Self::with_contract(provider, directory, question, specialist_id, None, ORIGINAL)
    }
    fn with_contract(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
        specialist_id: SpecialistId,
        required_role: Option<agents::TaskRangeRole>,
        original: &str,
    ) -> Self {
        Self::with_optional_contract(
            provider,
            directory,
            question,
            specialist_id,
            required_role,
            original,
            0,
            None,
            None::<
                fn(
                    &tauri::AppHandle<tauri::test::MockRuntime>,
                    &AppState,
                    &mut StartInvestigationRequest,
                ),
            >,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_prepared_contract(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
        specialist_id: SpecialistId,
        required_role: Option<agents::TaskRangeRole>,
        original: &str,
        inventory_json: Option<fn(&str) -> String>,
        before_start: impl FnOnce(
            &tauri::AppHandle<tauri::test::MockRuntime>,
            &AppState,
            &mut StartInvestigationRequest,
        ),
    ) -> Self {
        Self::with_optional_contract(
            provider,
            directory,
            question,
            specialist_id,
            required_role,
            original,
            0,
            inventory_json,
            Some(before_start),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn with_padded_contract(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
        specialist_id: SpecialistId,
        required_role: Option<agents::TaskRangeRole>,
        original: &str,
        padding_nodes: usize,
    ) -> Self {
        Self::with_optional_contract(
            provider,
            directory,
            question,
            specialist_id,
            required_role,
            original,
            padding_nodes,
            None,
            None::<
                fn(
                    &tauri::AppHandle<tauri::test::MockRuntime>,
                    &AppState,
                    &mut StartInvestigationRequest,
                ),
            >,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn with_optional_contract(
        provider: &dyn LlmProvider,
        directory: tempfile::TempDir,
        question: &str,
        specialist_id: SpecialistId,
        required_role: Option<agents::TaskRangeRole>,
        original: &str,
        padding_nodes: usize,
        inventory_json: Option<fn(&str) -> String>,
        before_start: Option<
            impl FnOnce(
                &tauri::AppHandle<tauri::test::MockRuntime>,
                &AppState,
                &mut StartInvestigationRequest,
            ),
        >,
    ) -> Self {
        let (state, app_data, graph_before) = captured_fixture_state(
            directory.path(),
            original,
            specialist_id.uses_coverage_qualification(),
            inventory_json,
        );

        let inspection_requirements = required_role.map(|role| {
            let node = state
                .graph
                .lock()
                .unwrap()
                .nodes_with_label("BusinessRule")
                .unwrap()
                .remove(0);
            let fact = agents::TaskFactKey::Node { id: node.id };
            let description =
                crate::primary_source::describe(&state, &crate::task_evidence::graph_fact(&fact))
                    .unwrap();
            let target = description
                .ranges
                .iter()
                .find_map(|range| {
                    let (target, _guard) = super::super::inspection::resolve(
                        &state,
                        &fact,
                        &description.graph_snapshot_id,
                        &description.receipt_id,
                        range.index,
                    )
                    .unwrap();
                    (target.role == role).then_some(target)
                })
                .unwrap();
            vec![target]
        });
        let expected_graph_revision = inspection_requirements
            .as_ref()
            .map(|targets| targets[0].graph_snapshot_id.clone());
        let mut request = StartInvestigationRequest {
            schema_version: if specialist_id.uses_coverage_qualification() {
                3
            } else if inspection_requirements.is_some() {
                2
            } else {
                1
            },
            inspection_requirements,
            request_nonce: "worker-fixture".into(),
            specialist_id,
            question: question.into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision,
            expected_context_id: None,
            conversation_id: None,
            parent_id: None,
        };
        // Closed setup: no AppHandle exists or escapes during graph padding.
        if padding_nodes > 0 {
            let mut graph = state.graph.lock().unwrap();
            let expected = graph.read_snapshot().unwrap();
            let patch = core_graph::GraphPatch {
                upsert_nodes: (0..padding_nodes)
                    .map(|i| core_graph::Node {
                        id: format!("padding:{i}"),
                        label: "Module".into(),
                        props: json!({"name":format!("padding{i}")}),
                    })
                    .collect(),
                ..Default::default()
            };
            assert!(
                graph
                    .apply_patch_if_snapshot_matches(&expected, &patch)
                    .unwrap()
            );
        }
        let app = FixtureApp::new(state);
        let state = app.state::<AppState>();
        if let Some(before_start) = before_start {
            before_start(app.handle(), &state, &mut request);
        }
        request.validate().unwrap();
        let start_time = Instant::now();
        let started = state
            .jobs
            .lock()
            .unwrap()
            .start_investigation(
                &request,
                Some(&descriptor(provider, InvestigationProviderMode::Local).unwrap()),
            )
            .unwrap();
        let start_milliseconds = start_time.elapsed().as_millis();
        let id = started.detail.summary.investigation_id;
        let plan = state
            .jobs
            .lock()
            .unwrap()
            .claim_plan(
                started.detail.summary.job_id,
                crate::jobs::ClaimMode::StartQueued,
            )
            .unwrap();
        let reservation = state
            .job_execution_locks
            .try_reserve(plan.lock_target())
            .unwrap();
        let (_, execution) = state
            .jobs
            .lock()
            .unwrap()
            .claim_execution(&plan, reservation)
            .unwrap();
        let live = state.investigations.insert(&id).unwrap();
        Self {
            _directory: directory,
            app_data,
            app,
            request,
            id,
            execution,
            live,
            graph_before,
            start_milliseconds,
        }
    }

    fn run(&self, provider: Arc<dyn LlmProvider>) {
        self.app.run_legacy(
            self.id.clone(),
            self.request.clone(),
            self.execution.clone(),
            provider,
            self.live.clone(),
            Instant::now(),
        );
    }

    fn cancel_inside_call(&self, provider: &Scripted, clear_job: bool) {
        let app = self.app.handle().clone();
        let id = self.id.clone();
        *provider.cancel.lock().unwrap() = Some(Box::new(move || {
            let state = app.state::<AppState>();
            let mut jobs = state.jobs.lock().unwrap();
            assert!(jobs.investigation(&id).unwrap().summary.invocation_pending);
            jobs.cancel_investigation(&id).unwrap();
            if clear_job {
                jobs.clear_finished().unwrap();
            }
        }));
    }
}

#[test]
fn investigation_query_pages_include_evidence_inventory_and_preserve_continuation() {
    // AC-0182: the documented query must page the complete host response, not
    // fail because its core page left no room for retained-source inventories.
    struct Sink {
        usage: InvestigationUsage,
        publications: usize,
    }
    impl AcquisitionSink for Sink {
        fn usage(&self) -> &InvestigationUsage {
            &self.usage
        }
        fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
            panic!("query metadata must not read source bytes")
        }
        fn publish_input(
            &mut self,
            ledger: &InvestigationInputLedger,
            usage: InvestigationUsage,
        ) -> Result<(), HostError> {
            ledger.validate()?;
            self.usage = usage;
            self.publications += 1;
            Ok(())
        }
    }
    let provider = Scripted::new(Mode::Finish);
    let fixture = Fixture::new(&provider);
    let state = fixture.app.state::<AppState>();
    let mut context = FrozenContext::prepare(&state, &fixture.request, &[]).unwrap();
    let expected = context
        .snapshot
        .query(context_hub::QueryRequest {
            max_facts: 32,
            max_bytes: 32768,
            ..Default::default()
        })
        .unwrap();
    let mut query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: None,
        labels: vec![],
        max_facts: 12,
        max_bytes: 16384,
        cursor: None,
    };
    let mut sink = Sink {
        usage: InvestigationUsage::default(),
        publications: 0,
    };
    let mut seen = Vec::new();
    loop {
        // AC-0327: exercise the host owner model with real captured receipts,
        // including replacement paging and each growing prospective ledger.
        // Serialized output is a lower-bound check, not a measured peak/RSS.
        use crate::investigations::query_workspace::{self, QueryInputs};
        let base_wire = context_hub::logical::count(&context.ledger).unwrap();
        let bound = query_workspace::workspace(
            &context.policy,
            QueryInputs {
                graph: core_graph::SnapshotReadLimits::default(),
                coverage: core_graph::coverage::CoverageReadLimits::default(),
                scoped_repos: 1, // this fixture registers one captured repository
                scope_identity_bytes: 4096,
                ledger_wire: base_wire,
                request_wire: context_hub::logical::count(&query).unwrap() + 128,
                app_path_bytes: crate::sources::MAX_PATH_BYTES,
            },
        )
        .unwrap();
        context.query(&state, query.clone(), &mut sink).unwrap();
        let raw = context.query_pages.last().unwrap();
        assert!(
            bound.retained
                >= base_wire + context_hub::logical::count(&context.ledger).unwrap() + raw.len()
        );
        assert!(bound.peak >= bound.retained);
        assert!(
            context
                .ledger
                .selected_facts
                .iter()
                .any(|fact| fact.binding.is_some())
        );
        assert!(raw.len() <= query.max_bytes);
        let wrapper: Value = serde_json::from_str(raw).unwrap();
        let page: context_hub::QueryResponse =
            serde_json::from_value(wrapper["context"].clone()).unwrap();
        assert_eq!(
            page.facts.len(),
            wrapper["evidence_options"].as_array().unwrap().len()
        );
        if sink.publications == 1 {
            assert!(page.facts.len() < expected.facts.len());
            assert!(page.next_cursor.is_some());
        }
        seen.extend(page.facts);
        assert_eq!(context.ledger.selected_facts.len(), seen.len());
        assert_eq!(context.ledger.citations.len(), seen.len());
        let bindings = context
            .ledger
            .selected_facts
            .iter()
            .filter_map(|s| s.binding.as_ref())
            .map(|b| (&b.repo_key, &b.receipt_id))
            .collect::<std::collections::BTreeSet<_>>();
        let indexed = context
            .ledger
            .receipt_references
            .iter()
            .map(|r| (&r.repo_key, &r.receipt_id))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            indexed, bindings,
            "an omitted fact must not retain its receipt"
        );
        query.cursor = page.next_cursor.map(|c| InvestigationQueryCursor {
            snapshot_id: c.snapshot_id,
            selection_id: c.selection_id,
            offset: c.offset,
        });
        if query.cursor.is_none() {
            break;
        }
        assert!(sink.publications < 8);
    }
    assert_eq!(
        seen, expected.facts,
        "paging may neither skip nor duplicate facts"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

    // One core fact fits 6000 bytes; its complete evidence inventory does not.
    // Keep failure explicit, without silently skipping it or changing history.
    let before = context.ledger.clone();
    let publications = sink.publications;
    let pages = context.query_pages.len();
    query = InvestigationQuery {
        scope: InvestigationScope::All,
        kind: Some(InvestigationFactKind::Node),
        labels: vec!["BusinessRule".into()],
        max_facts: 1,
        max_bytes: 6000,
        cursor: None,
    };
    assert_eq!(
        context.query(&state, query, &mut sink),
        Err(HostError::LimitExceeded)
    );
    assert_eq!(context.ledger, before);
    assert_eq!(context.query_pages.len(), pages);
    assert_eq!(sink.publications, publications);
}

#[test]
fn investigation_worker_queries_reads_captured_definition_and_persists_cited_finish() {
    // AC-0182/0183/0184/0188/0191: actual parser receipts, graph selection,
    // source retention, provider input and durable coordinator loop together.
    assert_worker_cited_finish(SpecialistId::DomainAnalyst);
}

#[test]
fn investigation_worker_compact_disclosure_persists_exact_finish_and_reopens() {
    // AC-0234/0235: the production firewall seals disclosure, the invocation
    // persists it, and schema-aware historical validation reopens the finish.
    assert_worker_cited_finish(SpecialistId::DomainAnalystV3);
}

fn assert_worker_cited_finish(specialist: SpecialistId) {
    let provider = Arc::new(Scripted::new(Mode::Finish));
    let fixture = Fixture::with_specialist(
        provider.as_ref(),
        tempfile::tempdir().unwrap(),
        "Inspect the available guard evidence and its limits.",
        specialist,
    );
    fixture.run(provider.clone());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert_eq!(*provider.copied.lock().unwrap(), vec!["ok === false"]);
    let state = fixture.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&fixture.id).unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Completed);
    assert_eq!(detail.usage.model_invocations, 3);
    assert_eq!(detail.usage.tool_actions, 2);
    assert_eq!(detail.usage.evidence_requests, 1);
    assert_eq!(
        detail.usage.captured_validation_bytes,
        ORIGINAL.len() as u64
    );
    assert_eq!(detail.usage.reported_input_tokens, Some(300));
    // AC-0418: real provider reports survive exact saved-task reopen below.
    assert_eq!(detail.usage.reported_cache_read_tokens, Some(21));
    let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
    assert_eq!(
        result.schema_version,
        if specialist.uses_compact_disclosure() {
            2
        } else {
            1
        }
    );
    assert_eq!(
        result.disclosure_hash.is_some(),
        specialist.uses_compact_disclosure()
    );
    assert_eq!(
        result.invocation_step_id.is_some(),
        specialist.uses_compact_disclosure()
    );
    assert_eq!(result.findings[0].tier, Tier::Agentic);
    assert_eq!(
        result.findings[0].confidence_tier,
        ConfidenceTier::InferredWeak
    );
    let ledger = jobs.investigation_ledger(&fixture.id).unwrap().unwrap();
    let citation = ledger
        .citations
        .iter()
        .find(|citation| citation.citation_id == result.findings[0].citation_ids[0])
        .unwrap();
    assert_eq!(
        citation.role,
        Some(agents::TaskRangeRole::DefinitionInitializer)
    );
    assert!(matches!(
        citation.origin,
        InvestigationEvidenceOrigin::CapturedPrimarySource { .. }
    ));
    let events = jobs.investigation_events(&fixture.id, 0).unwrap().items;
    let reserve = events
        .iter()
        .position(|event| event.kind == InvestigationEventKind::EvidenceValidationReserved)
        .unwrap();
    let read_done = events
        .iter()
        .position(|event| {
            event.kind == InvestigationEventKind::ToolCompleted
                && event.tool == Some(InvestigationTool::ReadEvidence)
        })
        .unwrap();
    assert!(reserve < read_done);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence + 1 == pair[1].sequence)
    );
    drop(jobs);
    assert_eq!(
        serde_json::to_string(&state.graph.lock().unwrap().read_snapshot().unwrap()).unwrap(),
        fixture.graph_before
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("ok === false")
    );
    let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&fixture.id).unwrap(),
        Some(result)
    );
    assert!(state.investigations.get(&fixture.id).unwrap().is_none());
}

#[test]
fn investigation_worker_retains_late_finish_after_cancellation_and_job_cleanup() {
    // AC-0187/0191: the actual call returns after cancellation and deletion;
    // only its finish is admitted, against its unchanged original ledger.
    for specialist in [SpecialistId::DomainAnalyst, SpecialistId::DomainAnalystV3] {
        let provider = Arc::new(Scripted::new(Mode::CancelFinish));
        let fixture = Fixture::with_specialist(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
            specialist,
        );
        fixture.cancel_inside_call(provider.as_ref(), true);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert!(detail.summary.cancel_requested);
        assert!(detail.summary.has_result);
        assert!(!detail.summary.invocation_pending);
        assert!(jobs.get(detail.summary.job_id).is_err());
        assert!(jobs.investigation_result(&fixture.id).unwrap().is_some());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
        assert_eq!(detail.usage.tool_actions, 2);
        let result = jobs.investigation_result(&fixture.id).unwrap().unwrap();
        assert_eq!(
            result.disclosure_hash.is_some(),
            specialist.uses_compact_disclosure()
        );
        drop(jobs);
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(result)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    }
}

#[test]
fn investigation_worker_never_executes_a_late_tool_after_cancellation() {
    // AC-0187: a tool returned by an already-issued call is not new permission.
    for specialist in [SpecialistId::DomainAnalyst, SpecialistId::DomainAnalystV3] {
        let provider = Arc::new(Scripted::new(Mode::CancelTool));
        let fixture = Fixture::with_specialist(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
            specialist,
        );
        fixture.cancel_inside_call(provider.as_ref(), false);
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let jobs = state.jobs.lock().unwrap();
        let detail = jobs.investigation(&fixture.id).unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::Cancelled);
        assert_eq!(detail.usage.tool_actions, 1);
        assert_eq!(detail.usage.evidence_requests, 0);
        assert_eq!(detail.usage.evidence_items, 0);
        assert!(!detail.summary.has_result);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(provider.copied.lock().unwrap().is_empty());
    }
}

#[test]
fn investigation_worker_rejects_malformed_actions_without_model_repair() {
    // AC-0184/0185: invalid complete output does not buy another invocation.
    let provider = Arc::new(Scripted::new(Mode::Malformed));
    let fixture = Fixture::new(provider.as_ref());
    fixture.run(provider.clone());
    let state = fixture.app.state::<AppState>();
    let detail = state
        .jobs
        .lock()
        .unwrap()
        .investigation(&fixture.id)
        .unwrap();
    assert_eq!(detail.summary.status, InvestigationStatus::Failed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(detail.usage.tool_actions, 0);
    assert!(!detail.summary.has_result);
    assert_eq!(
        detail.error.as_deref(),
        Some("The completed provider response was not an admissible action.")
    );
    // AC-1403: the rejected response is kept, bounded and redacted, with the task.
    let kept = state
        .jobs
        .lock()
        .unwrap()
        .investigation_rejected_response(&fixture.id)
        .unwrap()
        .unwrap();
    let text = "```json\n{}\n```";
    assert_eq!(kept.step_id, "step-1");
    assert_eq!(kept.response_bytes, text.len() as u64);
    assert_eq!(kept.response_hash, core_prov::content_hash(text.as_bytes()));
    assert_eq!(
        kept.retention,
        agents::investigation::rejected_response::ExcerptRetention::Excerpt {
            text: text.into(),
            truncated: false,
            redactions: 0,
        }
    );
}

#[test]
fn investigation_worker_records_unknown_transport_outcome_without_replay() {
    // AC-0185/0187: reservations survive unknown transport, with absent usage.
    for specialist in [SpecialistId::DomainAnalyst, SpecialistId::DomainAnalystV3] {
        let provider = Arc::new(Scripted::new(Mode::Unknown));
        let fixture = Fixture::with_specialist(
            provider.as_ref(),
            tempfile::tempdir().unwrap(),
            "Inspect the available guard evidence and its limits.",
            specialist,
        );
        fixture.run(provider.clone());
        let state = fixture.app.state::<AppState>();
        let detail = state
            .jobs
            .lock()
            .unwrap()
            .investigation(&fixture.id)
            .unwrap();
        assert_eq!(detail.summary.status, InvestigationStatus::OutcomeUnknown);
        assert_eq!(detail.usage.model_invocations, 1);
        assert_eq!(detail.usage.generated_token_reservations, 2048);
        assert_eq!(detail.usage.reported_input_tokens, None);
        assert_eq!(detail.usage.reported_cache_read_tokens, None);
        assert!(!detail.summary.has_result);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let reopened = crate::jobs::JobStore::open(fixture.app_data.join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), detail);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}

// The acceptance group's tests (#1924): a core-only build leaves them out.
#[cfg(not(app_test_group = "core"))]
#[path = "acceptance.rs"]
mod acceptance;

/// Every saved event of `id`, page by page, within the 96-event contract.
pub(super) fn ordered_events(jobs: &crate::jobs::JobStore, id: &str) -> Vec<InvestigationEvent> {
    let mut items = Vec::new();
    let mut after = 0;
    loop {
        let page = jobs.investigation_events(id, after).unwrap();
        items.extend(page.items);
        assert!(items.len() <= 96, "Event journal exceeded the contract");
        if !page.has_more {
            return items;
        }
        assert!(
            page.next_sequence > after,
            "Event pagination did not advance"
        );
        after = page.next_sequence;
    }
}

#[path = "turn_tests.rs"]
mod turn_tests;

#[path = "inspection_tests.rs"]
mod inspection_tests;

#[path = "coverage_tests.rs"]
mod coverage_tests;

#[path = "completion_tests.rs"]
mod completion_tests;

pub(in crate::investigations) use completion_tests::{
    with_public_start_fixture, with_public_start_fixture_then,
};

#[path = "finding_review_integration_tests.rs"]
mod finding_review_integration_tests;

#[path = "refusal_diagnostics_tests.rs"]
mod refusal_diagnostics;

#[test]
fn public_start_fixture_retains_state_after_inline_run_and_owner_drop() {
    with_public_start_fixture_then(
        Arc::new(Scripted::new(Mode::Finish)),
        true,
        |app, _, _| app.clone(),
        |app, _| {
            assert!(app.try_state::<AppState>().is_some());
        },
    );
}

// AC-0430: all existing bounded codes preserve classification without raw errors.
#[test]
fn investigation_provider_failure_codes_preserve_terminal_semantics_and_bounds() {
    use InvestigationStopReason as Stop;
    let codes = [
        (FailureCode::Unsupported, Stop::ProviderFailure),
        (FailureCode::InvalidLimits, Stop::ProviderFailure),
        (FailureCode::InvalidInput, Stop::ProviderFailure),
        (FailureCode::InvalidProfile, Stop::ProviderFailure),
        (FailureCode::ProfileChanged, Stop::ProviderFailure),
        (FailureCode::EgressDenied, Stop::ProviderFailure),
        (FailureCode::ConsentRequired, Stop::ProviderFailure),
        (FailureCode::ConsentMismatch, Stop::ProviderFailure),
        (FailureCode::InputTooLarge, Stop::LimitExceeded),
        (FailureCode::RequestTooLarge, Stop::LimitExceeded),
        (FailureCode::ResponseTooLarge, Stop::LimitExceeded),
        (FailureCode::OutputTooLarge, Stop::LimitExceeded),
        (FailureCode::Cancelled, Stop::Cancelled),
        (FailureCode::OwnershipLost, Stop::Operational),
        (FailureCode::DeadlineExceeded, Stop::LimitExceeded),
        (FailureCode::Transport, Stop::ProviderFailure),
        (FailureCode::HttpStatus, Stop::ProviderFailure),
        (FailureCode::UnsupportedEncoding, Stop::ProviderFailure),
        (FailureCode::InvalidResponse, Stop::ProviderFailure),
        (FailureCode::IncompleteResponse, Stop::ProviderFailure),
        (FailureCode::OutputLimitReached, Stop::ProviderFailure),
        (FailureCode::Refused, Stop::ProviderFailure),
        (FailureCode::EmptyCompletion, Stop::ProviderFailure),
    ];
    let historical_max = Stop::FinishOnlyToolProposal.test_message().len();
    assert_eq!(historical_max, 74);
    let mut messages = std::collections::BTreeSet::new();
    for (code, expected) in codes {
        for outcome in [
            InvocationOutcome::NotDispatched,
            InvocationOutcome::ResponseReceived,
            InvocationOutcome::Unknown,
        ] {
            let error = BoundedCallError { code, outcome };
            let reason = provider_failure(error).reason().unwrap();
            assert_eq!(reason, Stop::BoundedProvider(error));
            assert_eq!(
                reason.terminal_class(),
                if outcome == InvocationOutcome::Unknown {
                    Stop::OutcomeUnknown
                } else {
                    expected
                }
            );
            assert!(reason.test_message().len() <= Stop::InvalidResponse.test_message().len());
            assert!(reason.test_message().len() <= historical_max);
            assert_eq!(
                reason
                    .test_message()
                    .starts_with("Provider outcome unknown: "),
                outcome == InvocationOutcome::Unknown
            );
            messages.insert(reason.test_message());
        }
    }
    assert_eq!(messages.len(), 46);
    let host = Failure::Host(HostError::LimitExceeded).reason().unwrap();
    let store = Failure::Store(InvestigationStoreError::Capacity)
        .reason()
        .unwrap();
    assert_eq!(host, Stop::HostLimit);
    assert_eq!(store, Stop::StoreCapacity);
    for reason in [host, store] {
        assert_eq!(reason.terminal_class(), Stop::LimitExceeded);
        assert!(reason.test_message().len() <= Stop::InvalidResponse.test_message().len());
        assert!(!messages.contains(reason.test_message()));
    }
    assert_ne!(host.test_message(), store.test_message());
    // Both acquisition callbacks use this typed round-trip on transition errors.
    assert_eq!(
        Failure::from(acquisition_error(Failure::Store(
            InvestigationStoreError::Capacity
        )))
        .reason(),
        Some(Stop::StoreCapacity)
    );
    assert_eq!(
        Failure::from(acquisition_error(Failure::Host(HostError::LimitExceeded))).reason(),
        Some(Stop::HostLimit)
    );
    struct CapacitySink(InvestigationUsage);
    impl AcquisitionSink for CapacitySink {
        fn usage(&self) -> &InvestigationUsage {
            &self.0
        }
        fn reserve_validation(&mut self, _: u64) -> Result<(), HostError> {
            panic!("metadata query must not reserve source validation")
        }
        fn publish_input(
            &mut self,
            _: &InvestigationInputLedger,
            _: InvestigationUsage,
        ) -> Result<(), HostError> {
            Err(acquisition_error(Failure::Stop(Stop::StoreCapacity)))
        }
    }
    let provider = Scripted::new(Mode::Finish);
    let fixture = Fixture::new(&provider);
    let state = fixture.app.state::<AppState>();
    let mut context = FrozenContext::prepare(&state, &fixture.request, &[]).unwrap();
    let before = context.ledger.fingerprint().unwrap();
    let error = context
        .query(
            &state,
            InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 16384,
                cursor: None,
            },
            &mut CapacitySink(InvestigationUsage::default()),
        )
        .unwrap_err();
    assert_eq!(error, HostError::StoreCapacity);
    assert_eq!(Failure::from(error).reason(), Some(Stop::StoreCapacity));
    assert_eq!(context.ledger.fingerprint().unwrap(), before);
}

// Reuse the existing AppHandle lifetime boundary for coordinator-only fixtures.
pub(in crate::investigations) fn with_response_app(
    state: AppState,
    test: impl FnOnce(&tauri::AppHandle<tauri::test::MockRuntime>),
) {
    let app = FixtureApp::new(state);
    test(app.handle());
}

// AC-0582: closed subcodes retain their first annotated boundary and terminal semantics.
// AC-0582: authenticated policy and optional text-free diagnostic regression.
#[test]
fn terminal_diagnostic_closed_phase_and_class() {
    use agents::investigation::{
        InvestigationFailurePhase as Phase, InvestigationOperationalClass as Class,
    };
    for (error, class) in [
        (InvestigationStoreError::Invalid, Class::StoreInvalid),
        (InvestigationStoreError::Missing, Class::StoreMissing),
        (InvestigationStoreError::Conflict, Class::StoreConflict),
        (InvestigationStoreError::Stale, Class::StoreStale),
        (
            InvestigationStoreError::Unavailable,
            Class::StoreUnavailable,
        ),
        (InvestigationStoreError::Ownership, Class::StoreOwnership),
        (InvestigationStoreError::Storage, Class::StoreStorage),
    ] {
        let reason = Failure::Store(error)
            .preparation_at(PreparationPhase::SourceReservation)
            .preparation_at(PreparationPhase::Continuation)
            .reason()
            .unwrap();
        assert_eq!(
            reason,
            InvestigationStopReason::OperationalAt {
                phase: Phase::SourceReservation,
                error_class: class
            }
        );
        assert_eq!(
            reason.terminal_class(),
            InvestigationStopReason::Operational
        );
        assert_eq!(
            reason.test_message(),
            InvestigationStopReason::Operational.test_message()
        );
    }
    assert_eq!(
        Failure::Host(HostError::Operational)
            .preparation_at(PreparationPhase::Candidate)
            .reason(),
        Some(InvestigationStopReason::OperationalAt {
            phase: Phase::Candidate,
            error_class: Class::HostOperational
        })
    );
    assert_eq!(
        Failure::Store(InvestigationStoreError::Cancelled)
            .preparation_at(PreparationPhase::Candidate)
            .reason(),
        Some(InvestigationStopReason::Cancelled)
    );
    assert_eq!(
        Failure::Store(InvestigationStoreError::Capacity)
            .preparation_at(PreparationPhase::Candidate)
            .reason(),
        Some(InvestigationStopReason::StoreCapacity)
    );
}
