//! Isolated actual host context and test-only durable invocation seeding.
use super::*;
use crate::jobs::investigations::{InvestigationResponseMeta, PendingInvestigationStep};

pub(super) struct Fixture {
    pub(super) state: AppState,
    pub(super) context: FrozenContext,
    pub(super) seed: seed::MetadataHostSeed,
    current: InvestigationInput,
    current_prepared: PreparedBoundedCompletion,
    clock: RunClock,
    pub(super) directory: tempfile::TempDir,
}
fn provider() -> OllamaProvider {
    OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap()
}
impl Fixture {
    pub(super) fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("rule.ts"), "export function enabled(ok: boolean) { if (ok === false) return false; return true; }\n").unwrap();
        let state = crate::registered_source_tests::app_state(directory.path());
        let source = state.sources.lock().unwrap().register_local(&root).unwrap();
        // Actual retained receipt/guard fixture; no application intake command.
        let input = state
            .primary_sources
            .prepare(&source, source.root(), &["client".into()])
            .unwrap();
        let (extraction, receipts) = adapters_lang_ts::captured::extract_file(
            input.capture.as_ref().unwrap().file("rule.ts").unwrap(),
            &adapters_lang_ts::SourceId {
                repo: &source.repo_key,
                commit: "workdir",
            },
        )
        .unwrap();
        assert!(!receipts.is_empty());
        state
            .primary_sources
            .persist(&input, &source, &receipts)
            .unwrap();
        let bindings = crate::primary_source::matching_bindings(&extraction, &receipts);
        let record = CoverageRecord {
            repo_key: source.repo_key.clone(), registered_source_id: Some(source.source_id.clone()),
            ingest_id: "host-fixture".into(), config_hash: "host-config".into(),
            inventory_json: json!({"inventory":"coverage@1", "repo":source.repo_key,
                "commit":"workdir", "layer_hints":[], "producers":[], "unsupported":[], "limited":false}).to_string(),
        };
        crate::load_into_graph_with_bindings(
            &mut state.graph.lock().unwrap(),
            &extraction,
            &source.repo_key,
            source.root(),
            "workdir",
            &bindings,
            Some(&record),
        )
        .unwrap();
        drop(input);
        let acquisition = AcquisitionRequest {
            specialist_id: SpecialistId::DomainAnalystV10,
            inspection_requirements: None,
            question: "Which retained metadata supports this behavior?".into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            expected_graph_revision: None,
            expected_context_id: None,
            conversation_id: None,
            parent_id: None,
            policy: std::sync::Arc::new(INVESTIGATION_V2_POLICY.clone()),
        };
        let context = FrozenContext::prepare_qualified_with_scope(
            &state,
            &acquisition,
            &[],
            1024 * 1024,
            || {
                Ok(vec![ScopedRepo {
                    repo_key: source.repo_key.clone(),
                    registered_source_id: Some(source.source_id.clone()),
                    config_hash: record.config_hash.clone(),
                    layer_hints: None,
                }])
            },
        )
        .unwrap();
        assert!(context.query_pages.is_empty());
        let proof = context.ledger.coverage.as_ref().unwrap();
        let mut obligation = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: proof.context_id.clone(),
                repo: source.repo_key,
                inventory_hash: proof.qualification.repos[0].inventory_hash.clone().unwrap(),
            },
        };
        let obligation_id = obligation.content_id().unwrap();
        if let Obligation::Inventory {
            obligation_id: id, ..
        } = &mut obligation
        {
            *id = obligation_id;
        }
        let request = StagedObligationRequest {
            schema_version: 7,
            request_nonce: "host-metadata".into(),
            specialist_id: "domain-analyst@10".into(),
            obligations: ObligationEnvelope {
                schema_version: 2,
                contract: "investigation-completion-obligations@2".into(),
                obligations: vec![obligation],
            },
            inspection_requirements: None,
            question: acquisition.question,
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: INVESTIGATION_V2_POLICY.profile().into(),
            expected_graph_revision: None,
            expected_context_id: proof.context_id.clone(),
            conversation_id: None,
            parent_id: None,
        };
        let profile = provider().bounded_profile().unwrap();
        let stored_provider = InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: profile.provider_id,
            model: profile.requested_model,
            endpoint: profile.endpoint_id,
            deployment: None,
            protocol_version: Some(profile.protocol_version),
            available: true,
            unavailable_reason: None,
        };
        let actual = RefCell::new(None);
        let seeded = seed::seed(
            &mut state.jobs.lock().unwrap(),
            &state.job_execution_locks,
            request,
            &stored_provider,
            &context.ledger,
            context.coverage_archive.as_ref().unwrap(),
            |detail, request, metadata| {
                let turn = InvestigationTurnBasis {
                    input_ledger_hash: context
                        .ledger
                        .fingerprint_with_policy(&context.policy)
                        .unwrap(),
                    previous_ledger: None,
                    completed_tool: None,
                };
                let current = InvestigationInput::prepare_v9_with_policy(
                    &request.question,
                    SpecialistId::DomainAnalystV10,
                    context.ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &detail.usage,
                    &turn,
                    request,
                    metadata,
                    &[],
                    CompletionCallMode::Choice,
                    0,
                    None,
                    &context.policy,
                )
                .unwrap();
                let action = current
                    .action(format!("{}:step-1", detail.summary.investigation_id))
                    .unwrap();
                let prepared = EgressFirewall::new(EgressPolicy::default())
                    .prepare_bounded(&provider(), &action, &CompletionLimits::default())
                    .unwrap();
                let disclosure = current
                    .seal_disclosure_with_policy(&prepared, &context.policy)
                    .unwrap()
                    .unwrap();
                let step = PendingInvestigationStep {
                    step_id: "step-1".into(),
                    payload_hash: prepared.preview().egress.payload_hash.clone(),
                    input_ledger_hash: turn.input_ledger_hash,
                    input_bytes: serde_json::to_vec(&action.payload).unwrap().len(),
                    generated_tokens: context.policy.limits().generated_tokens_per_invocation,
                    disclosure: Some(disclosure),
                };
                *actual.borrow_mut() = Some((current, prepared));
                step
            },
        );
        let (current, current_prepared) = actual.into_inner().unwrap();
        assert!(seeded.metadata.completed_attempt_ids.is_empty());
        Self {
            directory,
            state,
            context,
            seed: seeded,
            current,
            current_prepared,
            clock: RunClock::new(Instant::now() - Duration::from_secs(1)),
        }
    }
    pub(super) fn prepare(&self, kind: PreparedContextReadKind) -> Publication {
        let (prepared, original, tool, preview_kind) = match kind {
            PreparedContextReadKind::Artifact => {
                unreachable!("artifact reads are tested separately")
            }
            PreparedContextReadKind::Query => {
                let query = InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: None,
                    labels: vec![],
                    max_facts: MAX_QUERY_FACTS,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                };
                let prepared = self
                    .context
                    .prepare_query(&self.state, query.clone())
                    .unwrap();
                assert!(!prepared.ledger().receipt_references.is_empty());
                (
                    prepared,
                    json!({"type":"query_context","context_id":self.seed.request.expected_context_id,"query":query}),
                    InvestigationTool::QueryContext,
                    ContextPreviewKind::Query,
                )
            }
            PreparedContextReadKind::Inventory => {
                let Obligation::Inventory { target, .. } =
                    &self.seed.request.obligations.obligations[0]
                else {
                    unreachable!()
                };
                let read = InvestigationCoverageRead {
                    context_id: target.context_id.clone(),
                    repo: target.repo.clone(),
                    inventory_hash: target.inventory_hash.clone(),
                    max_bytes: MAX_QUERY_BYTES,
                };
                (
                    self.context.prepare_inventory(read.clone()).unwrap(),
                    json!({"type":"read_coverage_inventory","context_id":read.context_id,"repo":read.repo,
                        "inventory_hash":read.inventory_hash,"max_bytes":read.max_bytes}),
                    InvestigationTool::ReadCoverageInventory,
                    ContextPreviewKind::Inventory,
                )
            }
        };
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: self.seed.step.step_id.clone(),
            response_hash: content_hash(b"complete scripted metadata action"),
            action_hash: content_hash(&serde_json::to_vec(&original).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(10),
            reported_output_tokens: Some(5),
            reported_cache_read_tokens: None,
            active_milliseconds: 1,
        };
        let (admitted, metadata) = seed::prospective(
            &self.state.jobs.lock().unwrap(),
            &self.seed.detail.summary.investigation_id,
            &response,
            prepared.ledger(),
            tool,
        );
        let mut usage = admitted.clone();
        usage.tool_actions += 1;
        usage.selected_facts = prepared.ledger().selected_facts.len();
        usage.active_milliseconds = self.clock.active_milliseconds();
        let view = self.context.prospective_context(&prepared).unwrap();
        let turn = InvestigationTurnBasis {
            input_ledger_hash: view
                .ledger
                .fingerprint_with_policy(&self.context.policy)
                .unwrap(),
            previous_ledger: Some(self.context.ledger.clone()),
            completed_tool: Some(tool),
        };
        let candidate = InvestigationInput::prepare_v9_with_policy(
            &self.seed.request.question,
            SpecialistId::DomainAnalystV10,
            view.ledger.clone(),
            &view.query_pages,
            view.evidence,
            &[],
            &usage,
            &turn,
            &self.seed.request,
            &metadata,
            &view.inventory_details,
            CompletionCallMode::Choice,
            0,
            None,
            &self.context.policy,
        )
        .unwrap();
        let action_id = format!("{}:step-2", self.seed.detail.summary.investigation_id);
        let candidate_prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(
                &provider(),
                &candidate.action(action_id.clone()).unwrap(),
                &CompletionLimits::default(),
            )
            .unwrap();
        let proof = self
            .current
            .certify_context_preview_v9_with_policy(
                &candidate,
                preview_kind,
                prepared.raw(),
                &admitted,
                &provider().bounded_profile().unwrap(),
                &CompletionLimits::default(),
                &action_id,
                &self.context.policy,
            )
            .unwrap();
        Publication {
            prepared,
            original,
            response,
            candidate,
            candidate_prepared,
            proof,
            kind: preview_kind,
            active_milliseconds: usage.active_milliseconds,
        }
    }
    pub(super) fn publish(
        &mut self,
        publication: Publication,
        revision: u64,
    ) -> Result<InvestigationDetail, MetadataPublicationError> {
        let admission = MetadataAdmission {
            response: &publication.response,
            original_action: &publication.original,
            current: &self.current,
            candidate: &publication.candidate,
            current_prepared: &self.current_prepared,
            candidate_prepared: &publication.candidate_prepared,
            proof: &publication.proof,
            kind: publication.kind,
            active_milliseconds: publication.active_milliseconds,
        };
        self.context.publish_admitted_metadata(
            &self.state,
            &self.seed.execution,
            revision,
            &self.clock,
            publication.prepared,
            &admission,
        )
    }
    pub(super) fn reopened(&self) -> (InvestigationDetail, InvestigationInputLedger, bool) {
        let store = crate::jobs::JobStore::open(self.directory.path().join("state.db")).unwrap();
        seed::inspect(&store, &self.seed.detail.summary.investigation_id)
    }
}
pub(super) struct Publication {
    pub(super) prepared: PreparedContextRead,
    original: Value,
    response: InvestigationResponseMeta,
    candidate: InvestigationInput,
    candidate_prepared: PreparedBoundedCompletion,
    proof: ContextPreviewBound,
    kind: ContextPreviewKind,
    active_milliseconds: u64,
}
