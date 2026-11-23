//! Real frozen query/archive preparation, actual firewall sealing and owner Tx;
//! no provider dispatch, source IO, public family activation or graph writes.
use super::*;
use completion::{InternalFixture, internal_fixture_with_step_preparer};
use context_hub::coverage::{CoverageContext, QualifiedRequest};
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, LlmProvider, OllamaProvider};
use serde_json::json;
use std::{cell::RefCell, sync::Arc, time::Duration};

struct Fixture {
    saved: InternalFixture,
    current: InvestigationInput,
    current_prepared: PreparedBoundedCompletion,
    candidate: InvestigationInput,
    candidate_prepared: PreparedBoundedCompletion,
    proof: ContextPreviewBound,
    raw: String,
    original: Value,
    response: InvestigationResponseMeta,
    kind: ContextPreviewKind,
    active_milliseconds: u64,
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
    fn new(kind: ContextPreviewKind) -> Self {
        let actual = RefCell::new(None);
        let profile = provider().bounded_profile().unwrap();
        let saved = internal_fixture_with_step_preparer(
            |_, task, _, _| {
                task.detail.provider = InvestigationProvider {
                    mode: InvestigationProviderMode::Local,
                    provider_id: profile.provider_id.clone(),
                    model: profile.requested_model.clone(),
                    endpoint: profile.endpoint_id.clone(),
                    deployment: None,
                    protocol_version: Some(profile.protocol_version.clone()),
                    available: true,
                    unavailable_reason: None,
                };
                vec![]
            },
            |task, request, ledger, metadata, pages| {
                assert!(pages.is_empty());
                let turn = InvestigationTurnBasis {
                    input_ledger_hash: ledger
                        .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap(),
                    previous_ledger: None,
                    completed_tool: None,
                };
                let input = InvestigationInput::prepare_v9_with_policy(
                    &request.question,
                    task.detail.summary.specialist_id,
                    ledger.clone(),
                    &[],
                    &[],
                    &[],
                    &task.detail.usage,
                    &turn,
                    request,
                    metadata,
                    &[],
                    CompletionCallMode::Choice,
                    0,
                    None,
                    &INVESTIGATION_V2_POLICY,
                )
                .unwrap();
                let step_id = "step-1".to_string();
                let action = input
                    .action(format!(
                        "{}:{step_id}",
                        task.detail.summary.investigation_id
                    ))
                    .unwrap();
                let prepared = EgressFirewall::new(EgressPolicy::default())
                    .prepare_bounded(&provider(), &action, &CompletionLimits::default())
                    .unwrap();
                let disclosure = input
                    .seal_disclosure_with_policy(&prepared, &INVESTIGATION_V2_POLICY)
                    .unwrap();
                let step = PendingInvestigationStep {
                    step_id,
                    payload_hash: prepared.preview().egress.payload_hash.clone(),
                    input_ledger_hash: ledger
                        .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                        .unwrap(),
                    input_bytes: encode(&action.payload, MAX_INPUT_BYTES).unwrap().len(),
                    generated_tokens: CompletionLimits::default().values().max_output_tokens,
                    disclosure,
                };
                *actual.borrow_mut() = Some((input, prepared));
                step
            },
        );
        let (current, current_prepared) = actual.into_inner().unwrap();
        let mut ledger = saved.ledger.clone();
        ledger.revision += 1;
        let (raw, original, tool, read) = match kind {
            ContextPreviewKind::Artifact => {
                unreachable!("artifact admissions are tested separately")
            }
            ContextPreviewKind::Query => {
                let query = InvestigationQuery {
                    scope: InvestigationScope::All,
                    kind: None,
                    labels: vec![],
                    max_facts: 12,
                    max_bytes: MAX_QUERY_BYTES,
                    cursor: None,
                };
                let snapshot = Arc::new(context_hub::ContextSnapshot::new(vec![], vec![]).unwrap());
                assert_eq!(snapshot.id(), ledger.scope_snapshot_id);
                let context = CoverageContext::from_shared_snapshot(
                    snapshot,
                    ledger.coverage.as_ref().unwrap().qualification.clone(),
                )
                .unwrap();
                let page = context
                    .query(QualifiedRequest {
                        context_id: Some(saved.request.expected_context_id.clone()),
                        request: context_hub::QueryRequest {
                            scope: context_hub::QueryScope::All,
                            kind: None,
                            labels: vec![],
                            max_facts: query.max_facts,
                            max_bytes: query.max_bytes,
                            cursor: None,
                        },
                    })
                    .unwrap();
                let raw = json!({"context":page,"evidence_options":[]}).to_string();
                ledger.queries.push(InvestigationQueryManifest {
                    query: query.clone(),
                    response_hash: core_prov::content_hash(raw.as_bytes()),
                    response_bytes: raw.len(),
                    returned_facts: 0,
                    total_selected: 0,
                    has_more: false,
                });
                (
                    raw,
                    json!({"type":"query_context","context_id":saved.request.expected_context_id,"query":query}),
                    InvestigationTool::QueryContext,
                    None,
                )
            }
            ContextPreviewKind::Inventory => {
                let tx = saved.store.conn.unchecked_transaction().unwrap();
                let archive = coverage::load(&tx, &saved.basis.task_id).unwrap().unwrap();
                let Obligation::Inventory { target, .. } =
                    &saved.request.obligations.obligations[0]
                else {
                    panic!("inventory fixture")
                };
                let read = InvestigationCoverageRead {
                    context_id: target.context_id.clone(),
                    repo: target.repo.clone(),
                    inventory_hash: target.inventory_hash.clone(),
                    max_bytes: MAX_QUERY_BYTES,
                };
                let raw = serde_json::to_string(&archive.read(&read).unwrap()).unwrap();
                ledger
                    .coverage_reads
                    .push(InvestigationCoverageReadManifest {
                        request: read.clone(),
                        response_hash: core_prov::content_hash(raw.as_bytes()),
                        response_bytes: raw.len(),
                    });
                let original = json!({"type":"read_coverage_inventory","context_id":read.context_id,"repo":read.repo,"inventory_hash":read.inventory_hash,"max_bytes":read.max_bytes});
                let intent = ReadIntent::Inventory {
                    context_id: read.context_id,
                    repo: read.repo,
                    inventory_hash: read.inventory_hash,
                    max_bytes: read.max_bytes,
                };
                (
                    raw,
                    original,
                    InvestigationTool::ReadCoverageInventory,
                    Some(intent),
                )
            }
        };
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: saved.step.step_id.clone(),
            response_hash: core_prov::content_hash(b"complete bounded action response"),
            action_hash: core_prov::content_hash(&encode(&original, MAX_ACTION_BYTES).unwrap()),
            observed_model: None,
            reported_input_tokens: Some(10),
            reported_output_tokens: Some(5),
            reported_cache_read_tokens: None,
            active_milliseconds: saved.task.detail.usage.active_milliseconds + 1,
        };
        let mut metadata = saved.metadata.clone();
        let admitted = {
            let tx = saved.store.conn.unchecked_transaction().unwrap();
            let (_, usage) = transitions::validate_response(&tx, &saved.task, &response).unwrap();
            if let Some(read) = &read {
                let records = completion::derive_attempt_records(
                    &saved.task,
                    &saved.step.step_id,
                    &response,
                    read,
                    &AttemptOutcome::InventoryAcquired {
                        read_index: saved.ledger.coverage_reads.len(),
                    },
                    Some(&ledger),
                    metadata.completed_attempt_ids.len(),
                )
                .unwrap();
                metadata
                    .completed_attempt_ids
                    .extend(records.into_iter().map(|record| record.attempt_id));
            }
            usage
        };
        let active_milliseconds = admitted.active_milliseconds + 10;
        let mut usage = admitted.clone();
        usage.tool_actions += 1;
        usage.selected_facts = ledger.selected_facts.len();
        usage.active_milliseconds = active_milliseconds;
        let turn = InvestigationTurnBasis {
            input_ledger_hash: ledger
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                .unwrap(),
            previous_ledger: Some(saved.ledger.clone()),
            completed_tool: Some(tool),
        };
        let (pages, details) = if kind == ContextPreviewKind::Query {
            (vec![raw.as_str()], vec![])
        } else {
            (vec![], vec![raw.as_str()])
        };
        let candidate = InvestigationInput::prepare_v9_with_policy(
            &saved.request.question,
            saved.task.detail.summary.specialist_id,
            ledger,
            &pages,
            &[],
            &[],
            &usage,
            &turn,
            &saved.request,
            &metadata,
            &details,
            CompletionCallMode::Choice,
            0,
            None,
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
        let action_id = format!("{}:step-2", saved.basis.task_id);
        let candidate_prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(
                &provider(),
                &candidate.action(action_id.clone()).unwrap(),
                &CompletionLimits::default(),
            )
            .unwrap();
        let proof = current
            .certify_context_preview_v9_with_policy(
                &candidate,
                kind,
                &raw,
                &admitted,
                &profile,
                &CompletionLimits::default(),
                &action_id,
                &INVESTIGATION_V2_POLICY,
            )
            .unwrap();
        Self {
            saved,
            current,
            current_prepared,
            candidate,
            candidate_prepared,
            proof,
            raw,
            original,
            response,
            kind,
            active_milliseconds,
        }
    }
    fn publish(&mut self) -> Result<InvestigationDetail, InvestigationStoreError> {
        let input = MetadataAdmission {
            response: &self.response,
            original_action: &self.original,
            current: &self.current,
            candidate: &self.candidate,
            current_prepared: &self.current_prepared,
            candidate_prepared: &self.candidate_prepared,
            proof: &self.proof,
            kind: self.kind,
            active_milliseconds: self.active_milliseconds,
        };
        self.saved.store.admit_next_prompt_metadata(
            &self.saved._execution_guard,
            self.saved.task.detail.summary.revision,
            &input,
            &self.raw,
            |_| Ok(()),
        )
    }
    fn assert_unpublished(&self) {
        let tx = self.saved.store.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &self.saved.basis.task_id).unwrap();
        assert_eq!(
            encode(&saved, MAX_RECORD_BYTES).unwrap(),
            encode(&self.saved.task, MAX_RECORD_BYTES).unwrap()
        );
        for table in ["investigation_responses", "completion_attempts"] {
            let count: i64 = tx
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
                    [&self.saved.basis.task_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 0);
        }
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_inputs WHERE task_id=?1",
                [&self.saved.basis.task_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let events: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_events WHERE task_id=?1 AND sequence>?2",
                params![
                    self.saved.basis.task_id,
                    signed(self.saved.task.detail.summary.last_event_sequence).unwrap()
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 0);
    }
}

#[test]
fn metadata_admission_query_and_inventory_commit_atomic_exact_projection_and_reopen() {
    // AC-0287: real production metadata and sealed inputs, no model dispatch.
    for kind in [ContextPreviewKind::Query, ContextPreviewKind::Inventory] {
        let mut f = Fixture::new(kind);
        let next = f.publish().unwrap();
        assert_eq!(next.usage.tool_actions, 1);
        assert_eq!(next.usage.evidence_requests, 0);
        assert_eq!(next.usage.evidence_bytes, 0);
        assert_eq!(next.usage.active_milliseconds, f.active_milliseconds);
        assert!(matches!(f.publish(), Err(InvestigationStoreError::Stale)));
        let reopened = JobStore::open(f.saved.directory.path().join("state.db")).unwrap();
        let tx = reopened.conn.unchecked_transaction().unwrap();
        let saved = load_next_prompt_task_base(&tx, &f.saved.basis.task_id).unwrap();
        assert_eq!(saved.phase, Phase::Ready);
        assert!(saved.pending.is_none());
        assert!(
            saved
                .admission_state
                .as_ref()
                .unwrap()
                .current_call
                .is_none()
        );
        assert_eq!(
            storage::load_ledger(&tx, &saved).unwrap().unwrap(),
            *f.candidate.ledger()
        );
        let attempts = completion::attempts(&tx, &saved).unwrap();
        assert_eq!(
            attempts.len(),
            usize::from(kind == ContextPreviewKind::Inventory)
        );
        assert_eq!(
            completion::metadata_current_next_prompt(&tx, &saved)
                .unwrap()
                .completed_attempt_ids,
            attempts
                .iter()
                .map(|entry| entry.record.attempt_id.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            admission_journal::derive_current(&tx, &saved)
                .unwrap()
                .count,
            0
        );
    }
}

#[test]
fn metadata_admission_rebound_action_profile_elapsed_and_cas_leave_no_effects() {
    for mutation in [
        "action",
        "profile",
        "elapsed_backwards",
        "elapsed_budget",
        "cas",
        "cancel",
    ] {
        let mut f = Fixture::new(ContextPreviewKind::Query);
        match mutation {
            "action" => {
                f.original["query"]["max_facts"] = 1.into();
                f.response.action_hash =
                    core_prov::content_hash(&encode(&f.original, MAX_ACTION_BYTES).unwrap());
            }
            "profile" => {
                f.saved.task.detail.provider.model = "different-model".into();
                let tx = f.saved.store.conn.unchecked_transaction().unwrap();
                admission_journal::tests::save_task(&tx, &f.saved.task);
                tx.commit().unwrap();
            }
            "elapsed_backwards" => f.active_milliseconds = 0,
            "elapsed_budget" => {
                f.active_milliseconds = f.saved.task.detail.limits.active_seconds * 1000
            }
            "cas" => f.saved.task.detail.summary.revision -= 1,
            "cancel" => {
                f.saved.task.detail.summary.cancel_requested = true;
                f.saved.task.detail.summary.actions.can_cancel = false;
                let tx = f.saved.store.conn.unchecked_transaction().unwrap();
                admission_journal::tests::save_task(&tx, &f.saved.task);
                tx.commit().unwrap();
            }
            _ => unreachable!(),
        }
        let expected = match mutation {
            "elapsed_budget" => InvestigationStoreError::Capacity,
            "cas" => InvestigationStoreError::Stale,
            "cancel" => InvestigationStoreError::Cancelled,
            _ => InvestigationStoreError::Invalid,
        };
        assert_eq!(f.publish(), Err(expected), "{mutation}");
        if mutation == "cas" {
            f.saved.task.detail.summary.revision += 1;
        }
        f.assert_unpublished();
    }
}

#[test]
fn metadata_admission_failure_after_final_write_rolls_back_all_effects() {
    for kind in [ContextPreviewKind::Query, ContextPreviewKind::Inventory] {
        let mut f = Fixture::new(kind);
        FAIL_AFTER_METADATA_WRITE.with(|fault| fault.set(true));
        assert_eq!(f.publish(), Err(InvestigationStoreError::Capacity));
        f.assert_unpublished();
    }
}

#[test]
fn metadata_admission_retained_guard_failure_preserves_typed_error_and_zero_effects() {
    let mut f = Fixture::new(ContextPreviewKind::Inventory);
    let input = MetadataAdmission {
        response: &f.response,
        original_action: &f.original,
        current: &f.current,
        candidate: &f.candidate,
        current_prepared: &f.current_prepared,
        candidate_prepared: &f.candidate_prepared,
        proof: &f.proof,
        kind: f.kind,
        active_milliseconds: f.active_milliseconds,
    };
    let called = std::cell::Cell::new(0);
    assert_eq!(
        f.saved.store.admit_next_prompt_metadata(
            &f.saved._execution_guard,
            f.saved.task.detail.summary.revision,
            &input,
            &f.raw,
            |_| {
                called.set(called.get() + 1);
                Err(InvestigationStoreError::Stale)
            }
        ),
        Err(InvestigationStoreError::Stale)
    );
    assert_eq!(called.get(), 1);
    f.assert_unpublished();
}
