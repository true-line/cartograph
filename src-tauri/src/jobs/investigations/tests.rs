#[path = "effective_policy_tests.rs"]
mod effective_policy_tests;

#[path = "query_refusal_tests.rs"]
mod query_refusal_tests;

#[path = "failure_record_tests.rs"]
mod failure_record_tests;

use super::chat::StoredChatMessage;
use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use agents::{TaskFactKey, TaskFactSelection, TaskSourceAssociation};

#[path = "chat_tests.rs"]
mod chat_tests;

#[path = "held_recovery_tests.rs"]
mod held_recovery_tests;

struct Fixture {
    directory: tempfile::TempDir,
    store: JobStore,
    locks: JobExecutionLocks,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = JobStore::open(directory.path().join("state.db")).unwrap();
        let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
        Self {
            directory,
            store,
            locks,
        }
    }
    fn start(&mut self, nonce: &str, mode: InvestigationProviderMode) -> (String, JobExecution) {
        self.start_as(nonce, mode, SpecialistId::DomainAnalyst)
    }
    fn start_as(
        &mut self,
        nonce: &str,
        mode: InvestigationProviderMode,
        specialist: SpecialistId,
    ) -> (String, JobExecution) {
        let mut requested = request(nonce, mode);
        requested.specialist_id = specialist;
        let detail = self
            .store
            .start_investigation(&requested, Some(&provider(mode)))
            .unwrap()
            .detail;
        let id = detail.summary.investigation_id;
        let plan = self
            .store
            .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
            .unwrap();
        let reservation = self.locks.try_reserve(plan.lock_target()).unwrap();
        let (job, execution) = self.store.claim_execution(&plan, reservation).unwrap();
        assert_eq!(job.investigation_id.as_deref(), Some(id.as_str()));
        assert_eq!(
            self.store.investigation(&id).unwrap().summary.status,
            InvestigationStatus::Preparing
        );
        (id, execution)
    }
    fn prepare(
        &mut self,
        id: &str,
        execution: &JobExecution,
        with_receipt: bool,
    ) -> InvestigationInputLedger {
        let ledger = ledger(with_receipt);
        let detail = self.store.investigation(id).unwrap();
        let usage = InvestigationUsage {
            selected_facts: ledger.selected_facts.len(),
            ..detail.usage
        };
        self.store
            .advance_investigation(
                execution,
                detail.summary.revision,
                InvestigationTransition::InputPrepared {
                    coverage_archive: None,
                    ledger: Box::new(ledger.clone()),
                    usage,
                },
            )
            .unwrap();
        ledger
    }
}

fn request(nonce: &str, mode: InvestigationProviderMode) -> StartInvestigationRequest {
    StartInvestigationRequest {
        inspection_requirements: None,
        schema_version: 1,
        request_nonce: nonce.into(),
        specialist_id: SpecialistId::DomainAnalyst,
        question: "What evidence supports the current design?".into(),
        scope: InvestigationScope::All,
        provider_mode: mode,
        limit_profile: "investigation-v1".into(),
        expected_graph_revision: None,
        expected_context_id: None,
        conversation_id: None,
        parent_id: None,
    }
}
fn provider(mode: InvestigationProviderMode) -> InvestigationProvider {
    InvestigationProvider {
        mode,
        provider_id: match mode {
            InvestigationProviderMode::Local => "local:fixture",
            InvestigationProviderMode::Cloud => "cloud:fixture",
        }
        .into(),
        model: "fixture-model".into(),
        endpoint: match mode {
            InvestigationProviderMode::Local => "http://127.0.0.1:11434",
            InvestigationProviderMode::Cloud => "https://provider.invalid",
        }
        .into(),
        deployment: None,
        protocol_version: None,
        available: true,
        unavailable_reason: None,
    }
}
fn hash(label: &str) -> String {
    core_prov::content_hash(label.as_bytes())
}
fn ledger(with_receipt: bool) -> InvestigationInputLedger {
    let fact = TaskFactKey::Node {
        id: "symbol:fixture".into(),
    };
    let digest = format!("node-v1:{}", hash("fact"));
    let receipt = format!("ts-primary-v2:{}", hash("receipt"));
    let mut ledger = InvestigationInputLedger {
        coverage_reads: vec![],
        schema_version: 1,
        coverage: None,
        graph_snapshot_id: format!("context-v1:{}", hash("graph")),
        scope_snapshot_id: format!("context-v1:{}", hash("scope")),
        revision: 0,
        selected_facts: vec![TaskFactSelection {
            fact: fact.clone(),
            fact_digest: digest.clone(),
            binding: with_receipt.then(|| TaskSourceAssociation {
                repo_key: "repo:fixture".into(),
                receipt_id: receipt.clone(),
                emitted_fact_digest: digest.clone(),
            }),
        }],
        receipt_references: vec![],
        citations: vec![InvestigationCitation {
            citation_id: "citation:graph".into(),
            fact,
            fact_digest: digest,
            source: None,
            role: None,
            index: None,
            text_hash: None,
            origin: InvestigationEvidenceOrigin::GraphMetadata,
        }],
        queries: vec![InvestigationQueryManifest {
            query: InvestigationQuery {
                scope: InvestigationScope::All,
                kind: None,
                labels: vec![],
                max_facts: 1,
                max_bytes: 1024,
                cursor: None,
            },
            response_hash: hash("fixture-query-response"),
            response_bytes: 128,
            returned_facts: 1,
            total_selected: 1,
            has_more: false,
        }],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    if with_receipt {
        ledger
            .receipt_references
            .push(agents::investigation::InvestigationReceiptReference {
                source_id: "src_0123456789abcdef0123456789abcdef".into(),
                repo_key: "repo:fixture".into(),
                receipt_id: receipt,
            });
    }
    ledger.validate().unwrap();
    ledger
}
fn step(ledger: &InvestigationInputLedger, index: u32) -> PendingInvestigationStep {
    PendingInvestigationStep {
        step_id: format!("step:{index}"),
        payload_hash: hash(&format!("payload:{index}")),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_bytes: 1024,
        generated_tokens: 2048,
        disclosure: None,
    }
}
fn begin(
    fixture: &mut Fixture,
    id: &str,
    execution: &JobExecution,
    step: PendingInvestigationStep,
) -> InvestigationDetail {
    let detail = fixture.store.investigation(id).unwrap();
    fixture
        .store
        .advance_investigation(
            execution,
            detail.summary.revision,
            InvestigationTransition::BeginInvocation { step },
        )
        .unwrap()
}
fn response(step_id: &str, active_milliseconds: u64) -> InvestigationResponseMeta {
    InvestigationResponseMeta {
        finish_feedback_witness: None,
        finish_telemetry: None,
        step_id: step_id.into(),
        response_hash: hash("response"),
        action_hash: hash("action"),
        observed_model: Some("fixture-model".into()),
        reported_input_tokens: None,
        reported_output_tokens: None,
        reported_cache_read_tokens: None,
        active_milliseconds,
    }
}
fn finish(id: &str, ledger: &InvestigationInputLedger) -> InvestigationResult {
    InvestigationResult::admit(
        id,
        ledger,
        &InvestigationAction::Finish {
            findings: vec![ProposedInvestigationFinding {
                claim_kind: InvestigationClaimKind::InferredInterpretation,
                title: "Bounded support".into(),
                statement: "The selected metadata suggests a responsibility worth examining."
                    .into(),
                citation_ids: vec!["citation:graph".into()],
                limitations: vec!["Implementation behavior remains unverified.".into()],
            }],
            knowledge_completeness: KnowledgeCompleteness::Partial,
            limitations: vec!["The investigation covers the selected input only.".into()],
        },
        std::iter::empty(),
        Some("fixture-model".into()),
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap()
}

#[test]
fn investigation_start_deduplicates_concurrently_before_availability_and_capacity() {
    // AC-0181/0188: two independent connections contend on the same request,
    // producing one task/conversation/job, even when the start acknowledgment is lost.
    let mut fixture = Fixture::new();
    let other = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let second = barrier.clone();
    let thread = std::thread::spawn(move || {
        let mut other = other;
        second.wait();
        let (result, diagnostic) = start_diagnostics::capture(|| {
            other.start_investigation(
                &request("same", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local)),
            )
        });
        result.unwrap_or_else(|error| panic!("concurrent start: {error:?}; {diagnostic:?}"))
    });
    barrier.wait();
    let (result, diagnostic) = start_diagnostics::capture(|| {
        fixture.store.start_investigation(
            &request("same", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
    });
    let first =
        result.unwrap_or_else(|error| panic!("concurrent start: {error:?}; {diagnostic:?}"));
    let second = thread.join().unwrap();
    assert_eq!(
        first.detail.summary.investigation_id,
        second.detail.summary.investigation_id
    );
    assert_ne!(first.created, second.created);
    assert_eq!(fixture.store.list().unwrap().len(), 1);
    assert_eq!(
        fixture
            .store
            .conn
            .query_row("SELECT count(*) FROM investigation_chats", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_chat_messages",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    fixture
        .store
        .start_investigation(
            &request("second", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .start_investigation(
                &request("third", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local))
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    let prior = fixture
        .store
        .start_investigation(&request("same", InvestigationProviderMode::Local), None)
        .unwrap();
    assert!(!prior.created);
    let mut changed = request("same", InvestigationProviderMode::Local);
    changed.question = "Different input".into();
    assert_eq!(
        fixture
            .store
            .start_investigation(&changed, None)
            .unwrap_err(),
        InvestigationStoreError::Conflict
    );
}

#[test]
fn investigation_execution_attachment_rolls_back_job_claim_on_storage_failure() {
    // AC-0181: Job claim updates happen before the coordinator attachment hook,
    // and any hook failure rolls both the running status and generation back.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("atomic", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    fixture.store.conn.execute("CREATE TRIGGER unexpected_attachment_trigger BEFORE INSERT ON investigation_events BEGIN SELECT RAISE(IGNORE); END",[]).unwrap();
    let plan = fixture
        .store
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    assert!(fixture.store.claim_execution(&plan, reservation).is_err());
    let state:(String,i64)=fixture.store.conn.query_row("SELECT j.status,a.generation FROM jobs j JOIN job_attempts a ON a.job_id=j.id WHERE j.id=?1",[detail.summary.job_id],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(state, ("queued".into(), 0));
    fixture
        .store
        .conn
        .execute("DROP TRIGGER unexpected_attachment_trigger", [])
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation(&detail.summary.investigation_id)
            .unwrap()
            .summary
            .status,
        InvestigationStatus::Queued
    );
    let plan = fixture
        .store
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = fixture.store.claim_execution(&plan, reservation).unwrap();
    let task = load_task(&fixture.store.conn, &detail.summary.investigation_id).unwrap();
    let identity = task.execution.unwrap();
    assert_eq!(identity.owner, execution.inner.owner);
    assert_eq!(identity.generation, execution.inner.generation);
    assert_eq!(task.phase, Phase::Preparing);
}

#[test]
fn investigation_cancelled_finish_survives_job_cleanup_restart_and_original_basis() {
    // AC-0187/0188: the response uses its admitted original ledger after cancel
    // and cleanup. History is independent of Jobs, raw questions are redacted,
    // and observed cooperative time overshoot remains visible rather than clamped.
    let mut fixture = Fixture::new();
    let mut request = request("late-finish", InvestigationProviderMode::Local);
    let canary = "ghp_abcdefgh1234_private_question_canary";
    request.question = format!("Inspect this question token {canary}");
    let created = fixture
        .store
        .start_investigation(&request, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap()
        .detail;
    let id = created.summary.investigation_id;
    let plan = fixture
        .store
        .claim_plan(created.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = fixture.store.claim_execution(&plan, reservation).unwrap();
    let ledger = fixture.prepare(&id, &execution, true);
    let started = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    fixture.store.cancel(started.summary.job_id).unwrap();
    assert_eq!(fixture.store.clear_finished().unwrap(), 1);
    assert!(fixture.store.get(started.summary.job_id).is_err());
    let result = finish(&id, &ledger);
    let current = fixture.store.investigation(&id).unwrap();
    assert_eq!(
        storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap()
            - storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap(),
        chat::RESULT_REFERENCE_SLOT_BYTES
    );
    chat::fail_after_result_reference_insert_once();
    assert_eq!(
        fixture.store.advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 600_123),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        ),
        Err(InvestigationStoreError::Storage)
    );
    assert!(!fixture.store.investigation(&id).unwrap().summary.has_result);
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_results WHERE task_id=?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0,
        "result and chat reference share one rollback boundary"
    );
    assert_eq!(
        chat::read_for_task(&fixture.store.conn, &id)
            .unwrap()
            .messages
            .len(),
        1
    );
    let current = fixture.store.investigation(&id).unwrap();
    let completed = fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 600_123),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    assert_eq!(completed.summary.status, InvestigationStatus::Completed);
    assert_eq!(
        storage::stored_bytes(&fixture.store.conn, Some(&id)).unwrap(),
        storage::actual_stored_bytes(&fixture.store.conn, Some(&id)).unwrap(),
        "committed result reference replaces the held slot"
    );
    assert!(completed.summary.cancel_requested && completed.summary.has_result);
    assert_eq!(completed.usage.active_milliseconds, 600_123);
    let mut reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
    let transcript = chat::read_for_task(&reopened.conn, &id).unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].ordinal, 0);
    assert_eq!(transcript.messages[1].ordinal, 1);
    assert!(matches!(
        &transcript.messages[1].message,
        StoredChatMessage::ResultReference { task_id, result_id, .. }
            if task_id == &id && result_id == &reopened.investigation_result(&id).unwrap().unwrap().result_id
    ));
    reopened.clear_finished().unwrap();
    assert_eq!(
        chat::read_for_task(&reopened.conn, &id).unwrap(),
        transcript
    );
    assert_eq!(reopened.investigation_ledger(&id).unwrap(), Some(ledger));
    assert!(reopened.list().unwrap().is_empty());
    let serialized = serde_json::to_string(&reopened.investigation(&id).unwrap()).unwrap();
    assert!(!serialized.contains(canary));
    assert!(!serialized.contains("\"owner\":"));
    assert!(!serialized.contains(&execution.inner.owner));
    let events = reopened.investigation_events(&id, 0).unwrap();
    assert!(
        events
            .items
            .iter()
            .any(|event| event.kind == InvestigationEventKind::ResultPersisted)
    );
    assert!(!serde_json::to_string(&events).unwrap().contains(canary));
    for table in [
        "investigation_events",
        "investigation_inputs",
        "investigation_results",
    ] {
        assert!(
            fixture
                .store
                .conn
                .execute(&format!("DELETE FROM {table} WHERE task_id=?1"), [&id])
                .is_err()
        );
    }
}

// AC-0375: an in-flight task from v3 remains finishable after migration without
// fabricating a chat for history that predates the new storage contract.
#[test]
fn legacy_v3_inflight_investigation_finishes_without_chat_backfill() {
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("legacy-chat-finish", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    let started = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    storage::version_three_fixture(&fixture.store.conn);
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        fixture.store.investigation(&id).unwrap().summary.revision,
        started.summary.revision
    );
    let result = finish(&id, &ledger);
    let current = fixture.store.investigation(&id).unwrap();
    fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 7),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    assert_eq!(
        fixture.store.investigation_result(&id).unwrap(),
        Some(result)
    );
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_chats WHERE task_id=?1",
                [&id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

// AC-0375: v3 tasks and immutable bodies survive the additive chat migration;
// historical tasks do not receive fabricated chat or message rows.
#[test]
fn durable_chat_v3_migration_preserves_task_bytes_without_backfill() {
    let mut fixture = Fixture::new();
    let id = fixture
        .store
        .start_investigation(
            &request("chat-v3-legacy", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary
        .investigation_id;
    storage::version_three_fixture(&fixture.store.conn);
    let original: (Vec<u8>, String) = fixture
        .store
        .conn
        .query_row(
            "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    fixture.store = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(fixture.store.investigation_chat(&id).unwrap(), None);
    let migrated: (Vec<u8>, String) = fixture
        .store
        .conn
        .query_row(
            "SELECT record,record_hash FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(migrated, original);
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_chats WHERE task_id=?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture
            .store
            .conn
            .query_row(
                "SELECT count(*) FROM investigation_chat_messages WHERE task_id=?1",
                [&id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        fixture.store.investigation(&id).unwrap().summary.status,
        InvestigationStatus::Queued
    );
}

#[test]
fn investigation_exact_step_consent_is_single_use_and_decline_never_changes_provider() {
    // AC-0186: a safe identity can wake the owner but cannot reconstruct its
    // preview. Matching approval and invocation budget consumption share a CAS.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("cloud", InvestigationProviderMode::Cloud);
    let ledger = fixture.prepare(&id, &execution, false);
    let step = step(&ledger, 1);
    let detail = fixture.store.investigation(&id).unwrap();
    let waiting = fixture
        .store
        .advance_investigation(
            &execution,
            detail.summary.revision,
            InvestigationTransition::AwaitConsent { step: step.clone() },
        )
        .unwrap();
    assert_eq!(
        fixture.store.investigation_pending_step(&id).unwrap(),
        Some((waiting.summary.revision, step.clone(), false))
    );
    assert_eq!(
        fixture
            .store
            .approve_investigation(&id, waiting.summary.revision, &step.step_id, &hash("wrong"))
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(
        fixture
            .store
            .investigation(&id)
            .unwrap()
            .usage
            .model_invocations,
        0
    );
    let approved = fixture
        .store
        .approve_investigation(
            &id,
            waiting.summary.revision,
            &step.step_id,
            &step.payload_hash,
        )
        .unwrap();
    assert_eq!(
        fixture.store.investigation_pending_step(&id).unwrap(),
        Some((approved.summary.revision, step.clone(), true))
    );
    assert!(
        fixture
            .store
            .approve_investigation(
                &id,
                waiting.summary.revision,
                &step.step_id,
                &step.payload_hash
            )
            .is_err()
    );
    let started = fixture
        .store
        .advance_investigation(
            &execution,
            approved.summary.revision,
            InvestigationTransition::BeginInvocation { step: step.clone() },
        )
        .unwrap();
    assert_eq!(started.usage.model_invocations, 1);
    assert_eq!(started.usage.generated_token_reservations, 2048);
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::BeginInvocation { step }
            )
            .is_err()
    );
    let (declined_id, declined_execution) =
        fixture.start("decline", InvestigationProviderMode::Cloud);
    let ledger = fixture.prepare(&declined_id, &declined_execution, false);
    let step = self::step(&ledger, 1);
    let detail = fixture.store.investigation(&declined_id).unwrap();
    let waiting = fixture
        .store
        .advance_investigation(
            &declined_execution,
            detail.summary.revision,
            InvestigationTransition::AwaitConsent { step: step.clone() },
        )
        .unwrap();
    let declined = fixture
        .store
        .decline_investigation(
            &declined_id,
            waiting.summary.revision,
            &step.step_id,
            &step.payload_hash,
        )
        .unwrap();
    assert_eq!(declined.summary.status, InvestigationStatus::Cancelled);
    assert_eq!(declined.provider.mode, InvestigationProviderMode::Cloud);
    assert_eq!(declined.usage.model_invocations, 0);
    assert!(
        fixture
            .store
            .investigation_pending_step(&declined_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn investigation_tool_and_repeated_file_validation_reservations_cannot_reset() {
    // AC-0183/0187: every attempted read reserves tool/request counts, and a
    // corrupt or repeated captured read spends full-file bytes before validation.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("read-budgets", InvestigationProviderMode::Local);
    let mut ledger = fixture.prepare(&id, &execution, false);
    for index in 1..=8 {
        let started = begin(&mut fixture, &id, &execution, step(&ledger, index));
        let action = fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::ActionAdmitted {
                    response: response(&format!("step:{index}"), u64::from(index)),
                    tool: Some(InvestigationTool::ReadEvidence),
                    result: None,
                },
            )
            .unwrap();
        let reading = fixture
            .store
            .advance_investigation(
                &execution,
                action.summary.revision,
                InvestigationTransition::BeginTool {
                    tool: InvestigationTool::ReadEvidence,
                },
            )
            .unwrap();
        assert_eq!(reading.usage.evidence_requests, index);
        let charged = fixture
            .store
            .advance_investigation(
                &execution,
                reading.summary.revision,
                InvestigationTransition::ChargeEvidence {
                    validation_bytes: 16 * 1024 * 1024,
                },
            )
            .unwrap();
        if index < 8 {
            ledger.revision += 1;
            let mut reset = charged.usage.clone();
            reset.captured_validation_bytes = 0;
            assert_eq!(
                fixture
                    .store
                    .advance_investigation(
                        &execution,
                        charged.summary.revision,
                        InvestigationTransition::InputAdmitted {
                            ledger: Box::new(ledger.clone()),
                            usage: reset
                        }
                    )
                    .unwrap_err(),
                InvestigationStoreError::Invalid
            );
            fixture
                .store
                .advance_investigation(
                    &execution,
                    charged.summary.revision,
                    InvestigationTransition::InputAdmitted {
                        ledger: Box::new(ledger.clone()),
                        usage: charged.usage,
                    },
                )
                .unwrap();
        } else {
            assert_eq!(charged.usage.captured_validation_bytes, 128 * 1024 * 1024);
            assert_eq!(
                fixture
                    .store
                    .advance_investigation(
                        &execution,
                        charged.summary.revision,
                        InvestigationTransition::ChargeEvidence {
                            validation_bytes: 1
                        }
                    )
                    .unwrap_err(),
                InvestigationStoreError::Capacity
            );
            let stopped = fixture
                .store
                .advance_investigation(
                    &execution,
                    charged.summary.revision,
                    InvestigationTransition::StopWithElapsed {
                        reason: InvestigationStopReason::LimitExceeded,
                        active_milliseconds: charged.usage.active_milliseconds,
                    },
                )
                .unwrap();
            assert_eq!(stopped.usage.tool_actions, 8);
            assert_eq!(stopped.usage.evidence_requests, 8);
            assert_eq!(stopped.usage.evidence_items, 0);
            assert_eq!(stopped.usage.captured_validation_bytes, 128 * 1024 * 1024);
        }
    }
}

#[test]
fn investigation_recovery_uses_retained_owner_after_cancel_and_job_cleanup() {
    // AC-0187: generic Job recovery cannot find this cancelled/cleared row.
    // The coordinator retains exact ownership, refuses a live lease, and only
    // marks uncertain dispatch after that lease is actually released.
    for pending in [false, true] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("recover", InvestigationProviderMode::Local);
        if pending {
            let ledger = fixture.prepare(&id, &execution, false);
            begin(&mut fixture, &id, &execution, step(&ledger, 1));
        }
        fixture.store.cancel_investigation(&id).unwrap();
        fixture.store.clear_finished().unwrap();
        assert!(fixture.store.recovery_candidates().unwrap().is_empty());
        let candidate = fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .remove(0);
        assert!(matches!(
            fixture.locks.try_reserve(candidate.lock_target()),
            Err(JobTransitionError::Busy)
        ));
        drop(execution);
        let reservation = fixture.locks.try_reserve(candidate.lock_target()).unwrap();
        let recovered = fixture
            .store
            .recover_investigation(&candidate, reservation)
            .unwrap()
            .unwrap();
        assert_eq!(
            recovered.summary.status,
            if pending {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Interrupted
            }
        );
        assert!(recovered.summary.cancel_requested);
        assert!(
            fixture
                .store
                .investigation_recovery_candidates()
                .unwrap()
                .is_empty()
        );
        assert!(
            !fixture
                .store
                .start_investigation(&request("recover", InvestigationProviderMode::Local), None)
                .unwrap()
                .created
        );
        let events = fixture.store.investigation_events(&id, 0).unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|event| event.kind == InvestigationEventKind::ModelStarted)
                .count(),
            usize::from(pending)
        );
    }
}

#[test]
fn investigation_ended_by_owner_death_is_clearable_without_relabelling_its_outcome() {
    // AC-0202: an investigation ended by owner death leaves an `interrupted`
    // Job the coordinator never resumes. Clearing removes that row only; the
    // task keeps its interrupted/outcome-unknown status and journal, and an
    // ordinary resumable interrupted Job is still kept.
    for pending in [false, true] {
        let mut fixture = Fixture::new();
        let resumable = fixture.store.enqueue("ingest:/resumable").unwrap().id;
        fixture.store.set_status(resumable, "interrupted").unwrap();
        let (id, execution) = fixture.start("owner-death", InvestigationProviderMode::Local);
        let job_id = execution.id();
        if pending {
            let ledger = fixture.prepare(&id, &execution, false);
            begin(&mut fixture, &id, &execution, step(&ledger, 1));
        }
        drop(execution);
        let candidate = fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .remove(0);
        let reservation = fixture.locks.try_reserve(candidate.lock_target()).unwrap();
        let recovered = fixture
            .store
            .recover_investigation(&candidate, reservation)
            .unwrap()
            .unwrap();
        let outcome = if pending {
            InvestigationStatus::OutcomeUnknown
        } else {
            InvestigationStatus::Interrupted
        };
        assert_eq!(recovered.summary.status, outcome);
        assert_eq!(fixture.store.get(job_id).unwrap().status, "interrupted");
        let events = fixture.store.investigation_events(&id, 0).unwrap();

        assert_eq!(fixture.store.clear_finished().unwrap(), 1);
        assert!(fixture.store.get(job_id).is_err());
        assert_eq!(fixture.store.get(resumable).unwrap().status, "interrupted");
        let kept = fixture.store.investigation(&id).unwrap();
        assert_eq!(kept.summary.status, outcome);
        assert_eq!(kept.summary.revision, recovered.summary.revision);
        assert!(!kept.summary.has_result && !kept.summary.cancel_requested);
        assert_eq!(fixture.store.investigation_events(&id, 0).unwrap(), events);
        assert_eq!(fixture.store.clear_finished().unwrap(), 0);
    }
}

#[test]
fn investigation_unclaimed_queue_is_not_abandoned_and_missing_claimed_lock_fails_closed() {
    // AC-0181/0187: the start→claim gap is not proof of death. An actually
    // claimed missing lock is an operational failure, never automatically recreated.
    let mut fixture = Fixture::new();
    let queued = fixture
        .store
        .start_investigation(
            &request("queued", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    assert!(
        fixture
            .store
            .investigation_recovery_candidates()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .store
            .investigation(&queued.summary.investigation_id)
            .unwrap()
            .summary
            .status,
        InvestigationStatus::Queued
    );
    fixture
        .store
        .cancel_investigation(&queued.summary.investigation_id)
        .unwrap();
    let (id, execution) = fixture.start("missing-lock", InvestigationProviderMode::Local);
    let job_id = execution.id();
    drop(execution);
    let candidate = fixture
        .store
        .investigation_recovery_candidates()
        .unwrap()
        .remove(0);
    let path = fixture
        .directory
        .path()
        .join("job-executions")
        .join(&fixture.store.namespace.value)
        .join(job_id.to_string());
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(
        fixture.locks.try_reserve(candidate.lock_target()),
        Err(JobTransitionError::LockUnavailable)
    ));
    assert!(!path.exists());
    assert_eq!(
        fixture.store.investigation(&id).unwrap().summary.status,
        InvestigationStatus::Preparing
    );
}

#[test]
fn investigation_selected_receipts_are_indexed_before_source_reads_and_validated() {
    // AC-0188/0189: a receipt identity is already supplied by the selected fact
    // inventory. It belongs in retention previews before any source text is read.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("refs", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    let source = &ledger.receipt_references[0];
    let uses = fixture
        .store
        .investigation_receipt_references(&source.source_id)
        .unwrap();
    assert_eq!(
        uses,
        vec![InvestigationReceiptUse {
            investigation_id: id.clone(),
            source_id: source.source_id.clone(),
            repo_key: source.repo_key.clone(),
            receipt_id: source.receipt_id.clone()
        }]
    );
    assert_eq!(
        fixture
            .store
            .investigation(&id)
            .unwrap()
            .usage
            .evidence_items,
        0
    );
    assert!(
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_refs SET receipt_id='invalid' WHERE task_id=?1",
                [&id]
            )
            .is_err()
    );
    fixture.store.cancel_investigation(&id).unwrap();
    fixture.store.clear_finished().unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_receipt_references(&source.source_id)
            .unwrap(),
        uses
    );
    assert_eq!(
        fixture.store.investigation_ledger(&id).unwrap(),
        Some(ledger)
    );
}

#[test]
fn investigation_history_is_bounded_stable_and_idempotent_after_full_admission() {
    // AC-0181/0188: a history cursor binds a high-water mark and conversation;
    // job cleanup is independent, and a full store still resolves prior nonces.
    let mut fixture = Fixture::new();
    for index in 0..55 {
        let detail = fixture
            .store
            .start_investigation(
                &request(
                    &format!("history:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let first = fixture.store.investigation_history(None, None).unwrap();
    assert_eq!(first.items.len(), 50);
    let cursor = first.next_cursor.unwrap();
    for index in 55..128 {
        let detail = fixture
            .store
            .start_investigation(
                &request(
                    &format!("history:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let second = fixture
        .store
        .investigation_history(None, Some(&cursor))
        .unwrap();
    assert_eq!(second.items.len(), 5);
    assert!(second.next_cursor.is_none());
    assert!(
        fixture
            .store
            .investigation_history(Some(&first.items[0].conversation_id), Some(&cursor))
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .start_investigation(
                &request("overflow", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local))
            )
            .unwrap_err(),
        InvestigationStoreError::Capacity
    );
    assert!(
        !fixture
            .store
            .start_investigation(
                &request("history:0", InvestigationProviderMode::Local),
                None
            )
            .unwrap()
            .created
    );
    assert_eq!(fixture.store.clear_finished().unwrap(), 128);
    assert_eq!(
        fixture
            .store
            .investigation_history(None, None)
            .unwrap()
            .items
            .len(),
        50
    );
    assert!(serde_json::to_vec(&second).unwrap().len() < 256 * 1024);
}

#[test]
fn investigation_history_conversation_keeps_forks_as_siblings() {
    let mut fixture = Fixture::new();
    let parent = fixture
        .store
        .start_investigation(
            &request("conversation-parent", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;

    let mut child_request = request("conversation-child", InvestigationProviderMode::Local);
    child_request.conversation_id = Some(parent.conversation_id.clone());
    child_request.parent_id = Some(parent.investigation_id.clone());
    let child = fixture
        .store
        .start_investigation(
            &child_request,
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    fixture
        .store
        .cancel_investigation(&child.investigation_id)
        .unwrap();

    let mut grandchild_request =
        request("conversation-grandchild", InvestigationProviderMode::Local);
    grandchild_request.conversation_id = Some(parent.conversation_id.clone());
    grandchild_request.parent_id = Some(child.investigation_id.clone());
    let grandchild = fixture
        .store
        .start_investigation(
            &grandchild_request,
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    fixture
        .store
        .cancel_investigation(&grandchild.investigation_id)
        .unwrap();

    let mut siblings = Vec::new();
    for index in 0..49 {
        let mut sibling_request = request(
            &format!("conversation-sibling-{index}"),
            InvestigationProviderMode::Local,
        );
        sibling_request.conversation_id = Some(parent.conversation_id.clone());
        sibling_request.parent_id = Some(parent.investigation_id.clone());
        let sibling = fixture
            .store
            .start_investigation(
                &sibling_request,
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail
            .summary;
        fixture
            .store
            .cancel_investigation(&sibling.investigation_id)
            .unwrap();
        siblings.push(sibling);
    }
    fixture
        .store
        .cancel_investigation(&parent.investigation_id)
        .unwrap();

    let mut unrelated_request = request("conversation-unrelated", InvestigationProviderMode::Local);
    unrelated_request.conversation_id = None;
    let unrelated = fixture
        .store
        .start_investigation(
            &unrelated_request,
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    fixture
        .store
        .cancel_investigation(&unrelated.investigation_id)
        .unwrap();

    let page = fixture
        .store
        .investigation_history(Some(&parent.conversation_id), None)
        .unwrap();
    assert_eq!(page.items.len(), 50);
    assert_eq!(
        page.items[0].investigation_id,
        siblings.last().unwrap().investigation_id
    );
    assert!(
        page.items
            .iter()
            .all(|item| item.conversation_id == parent.conversation_id)
    );
    assert!(
        !page
            .items
            .iter()
            .any(|item| item.investigation_id == unrelated.investigation_id)
    );
    let cursor = page.next_cursor.unwrap();
    assert!(
        fixture
            .store
            .investigation_history(Some("conversation:unknown"), None)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        fixture
            .store
            .investigation_history(Some("conversation:other"), Some(&cursor))
            .is_err()
    );
    let second = fixture
        .store
        .investigation_history(Some(&parent.conversation_id), Some(&cursor))
        .unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|item| item.investigation_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            child.investigation_id.as_str(),
            parent.investigation_id.as_str()
        ]
    );
    assert_eq!(
        grandchild.parent_id.as_deref(),
        Some(child.investigation_id.as_str())
    );
    assert_eq!(
        second.items[0].parent_id.as_deref(),
        Some(parent.investigation_id.as_str())
    );
    assert_eq!(second.items[1].parent_id, None);

    // AC-0374: the persisted conversation and its bounded cursor survive a real
    // store close/reopen; selected-turn reads remain independently addressable.
    let database_path = fixture.directory.path().join("state.db");
    drop(fixture.store);
    let reopened = JobStore::open(database_path).unwrap();
    let reopened_first = reopened
        .investigation_history(Some(&parent.conversation_id), None)
        .unwrap();
    assert_eq!(
        reopened_first
            .items
            .iter()
            .map(|item| item.investigation_id.as_str())
            .collect::<Vec<_>>(),
        page.items
            .iter()
            .map(|item| item.investigation_id.as_str())
            .collect::<Vec<_>>()
    );
    let reopened_cursor = reopened_first.next_cursor.unwrap();
    let reopened_second = reopened
        .investigation_history(Some(&parent.conversation_id), Some(&reopened_cursor))
        .unwrap();
    assert_eq!(
        reopened_second
            .items
            .iter()
            .map(|item| item.investigation_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            child.investigation_id.as_str(),
            parent.investigation_id.as_str()
        ]
    );
}

// AC-0408: the validated saved display name works without a completed answer.
#[test]
fn investigation_history_search_matches_saved_specialist_names() {
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("name-only", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let found = reopened
        .search_investigation_history("  DOMAIN ANALYST  ", None)
        .unwrap();
    assert_eq!(found.name_match_ids, [detail.summary.investigation_id]);
    assert!(found.answer_match_ids.is_empty() && !found.items[0].has_result);
    assert!(
        reopened
            .search_investigation_history("Domain analyst%", None)
            .unwrap()
            .items
            .is_empty()
    );
}

#[test]
fn investigation_history_search_finds_persisted_legacy_rows_beyond_first_page() {
    // AC-0368: search traverses retained summaries outside the loaded first page
    // and returns the exact saved family instead of upgrading legacy history.
    let mut fixture = Fixture::new();
    let mut oldest_id = String::new();
    for index in 0..60 {
        let mut requested = request(
            &format!("search-history:{index}"),
            InvestigationProviderMode::Local,
        );
        requested.question = if index == 0 {
            "Needle in an older saved question".into()
        } else {
            format!("Unrelated saved question {index}")
        };
        if index == 0 {
            requested.specialist_id = SpecialistId::EvidenceAuditorV1;
        }
        let detail = fixture
            .store
            .start_investigation(
                &requested,
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        if index == 0 {
            oldest_id = detail.summary.investigation_id.clone();
        }
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }

    let ordinary = fixture.store.investigation_history(None, None).unwrap();
    assert_eq!(ordinary.items.len(), 50);
    assert!(
        !ordinary
            .items
            .iter()
            .any(|item| item.investigation_id == oldest_id)
    );
    let found = fixture
        .store
        .search_investigation_history("  nEeDlE  ", None)
        .unwrap();
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].investigation_id, oldest_id);
    assert_eq!(found.items[0].schema_version, 1);
    assert_eq!(found.items[0].question, "Needle in an older saved question");
    assert!(found.next_cursor.is_none());
    let vertical_tab = fixture
        .store
        .search_investigation_history("\u{000b}Needle\u{000b}", None)
        .unwrap();
    assert_eq!(vertical_tab.items[0].investigation_id, oldest_id);
    // AC-0408: saved names remain searchable beyond the first loaded page.
    let names = fixture
        .store
        .search_investigation_history("Evidence auditor", None)
        .unwrap();
    assert_eq!(names.name_match_ids, [oldest_id.clone()]);
    let names = fixture
        .store
        .search_investigation_history("Domain analyst", None)
        .unwrap();
    assert_eq!(names.items.len(), 50);
    assert_eq!(names.name_match_ids.len(), 50);
    assert!(
        names
            .name_match_ids
            .iter()
            .all(|id| names.items.iter().any(|item| &item.investigation_id == id))
    );
    let rest = fixture
        .store
        .search_investigation_history("Domain analyst", names.next_cursor.as_deref())
        .unwrap();
    assert_eq!(rest.items.len(), 9);
    assert_eq!(rest.name_match_ids.len(), 9);
    assert!(
        rest.name_match_ids
            .iter()
            .all(|id| !names.name_match_ids.contains(id))
    );
    let role = fixture
        .store
        .search_investigation_history("EVIDENCE-AUDITOR@1", None)
        .unwrap();
    assert_eq!(role.items.len(), 1);
    assert_eq!(role.items[0].investigation_id, oldest_id);
    assert!(
        fixture
            .store
            .search_investigation_history("\"", None)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        fixture
            .store
            .search_investigation_history("\\", None)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        fixture.store.search_investigation_history(
            &" ".repeat(agents::investigation::MAX_QUESTION_BYTES + 1),
            None
        ),
        Err(InvestigationStoreError::Invalid)
    ));
}

#[test]
fn investigation_history_search_cursor_binds_term_and_fixed_high_water() {
    // AC-0368: search continuation is query-bound and never picks up newer rows.
    let mut fixture = Fixture::new();
    for index in 0..51 {
        let mut requested = request(
            &format!("search-page:{index}"),
            InvestigationProviderMode::Local,
        );
        requested.question = format!("Needle saved question {index}");
        let detail = fixture
            .store
            .start_investigation(
                &requested,
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let first = fixture
        .store
        .search_investigation_history("needle", None)
        .unwrap();
    assert_eq!(first.items.len(), 50);
    let cursor = first.next_cursor.unwrap();

    let mut newer = request("search-newer", InvestigationProviderMode::Local);
    newer.question = "Needle added after the search snapshot".into();
    let newer_id = fixture
        .store
        .start_investigation(&newer, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap()
        .detail
        .summary
        .investigation_id;
    fixture.store.cancel_investigation(&newer_id).unwrap();

    assert_eq!(
        fixture
            .store
            .search_investigation_history("different", Some(&cursor))
            .unwrap_err(),
        InvestigationStoreError::Invalid
    );
    let second = fixture
        .store
        .search_investigation_history("NEEDLE", Some(&cursor))
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(second.items[0].investigation_id, newer_id);
    assert!(second.next_cursor.is_none());
}

#[test]
fn investigation_active_history_finds_older_active_work_and_excludes_terminal() {
    // AC-0372: active selection scans persisted rows beyond the ordinary first
    // page and uses each row's current durable status.
    let mut fixture = Fixture::new();
    let active = fixture
        .store
        .start_investigation(
            &request("active-history-old", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    for index in 0..60 {
        let created = fixture
            .store
            .start_investigation(
                &request(
                    &format!("active-history-terminal:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&created.summary.investigation_id)
            .unwrap();
    }

    let ordinary = fixture.store.investigation_history(None, None).unwrap();
    assert_eq!(ordinary.items.len(), 50);
    assert!(
        ordinary
            .items
            .iter()
            .all(|item| item.investigation_id != active.investigation_id)
    );

    let active_page = fixture
        .store
        .investigation_history_selection(None, true, None)
        .unwrap();
    assert_eq!(active_page.items.len(), 1);
    assert_eq!(
        active_page.items[0].investigation_id,
        active.investigation_id
    );
    assert_eq!(active_page.items[0].status, InvestigationStatus::Queued);
    assert!(active_page.next_cursor.is_none());
}

#[test]
fn investigation_active_history_cursor_binds_selection_and_refreshes_current_status() {
    // AC-0372: active and ordinary cursors cannot cross selections, and the
    // query reflects current status after a task becomes terminal.
    let mut fixture = Fixture::new();
    let active_id = fixture
        .store
        .start_investigation(
            &request("active-history-oldest", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary
        .investigation_id;
    for index in 0..51 {
        let created = fixture
            .store
            .start_investigation(
                &request(
                    &format!("active-history-page:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        fixture
            .store
            .cancel_investigation(&created.summary.investigation_id)
            .unwrap();
    }
    let active_recent = fixture
        .store
        .start_investigation(
            &request("active-history-recent", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary
        .investigation_id;
    let ordinary = fixture.store.investigation_history(None, None).unwrap();
    let ordinary_cursor = ordinary.next_cursor.unwrap();
    assert!(
        fixture
            .store
            .investigation_history_selection(None, true, Some(&ordinary_cursor))
            .is_err()
    );
    let active_page = fixture
        .store
        .investigation_history_selection(None, true, None)
        .unwrap();
    assert_eq!(active_page.items.len(), 2);
    assert_eq!(active_page.items[0].investigation_id, active_recent);
    assert_eq!(active_page.items[1].investigation_id, active_id);
    assert!(active_page.next_cursor.is_none());
    let active_matching_search = fixture
        .store
        .investigation_history_selection(Some("EVIDENCE"), true, None)
        .unwrap();
    assert_eq!(active_matching_search.items.len(), 2);
    let active_nonmatching_search = fixture
        .store
        .investigation_history_selection(Some("no matching question"), true, None)
        .unwrap();
    assert!(active_nonmatching_search.items.is_empty());

    fixture.store.cancel_investigation(&active_id).unwrap();
    let refreshed = fixture
        .store
        .investigation_history_selection(None, true, None)
        .unwrap();
    assert_eq!(refreshed.items.len(), 1);
    assert_eq!(refreshed.items[0].investigation_id, active_recent);
    assert!(refreshed.next_cursor.is_none());
}

#[test]
fn investigation_specialist_activity_projects_saved_provider_within_existing_bounds() {
    // AC-0381: activity attribution comes from each validated saved task.
    let mut fixture = Fixture::new();
    let cases = [
        (
            InvestigationProviderMode::Local,
            "saved-local-provider",
            "saved-local-model",
        ),
        (
            InvestigationProviderMode::Cloud,
            "saved-cloud-provider",
            "saved-cloud-model",
        ),
    ];
    let mut ids = Vec::new();
    for (index, (mode, provider_id, model)) in cases.into_iter().enumerate() {
        let mut saved = provider(mode);
        saved.provider_id = provider_id.into();
        saved.model = model.into();
        let summary = fixture
            .store
            .start_investigation(
                &request(&format!("saved-provider:{index}"), mode),
                Some(&saved),
            )
            .unwrap()
            .detail
            .summary;
        // AC-0417: preserve all u64 digits and each unknown direction independently.
        let mut task = load_task(&fixture.store.conn, &summary.investigation_id).unwrap();
        task.detail.usage.reported_input_tokens = (index == 0).then_some(u64::MAX);
        task.detail.usage.reported_output_tokens = (index == 1).then_some(0);
        task.detail.usage.reported_cache_read_tokens = (index == 0).then_some(u64::MAX);
        storage::save_task(&fixture.store.conn, &task, summary.revision, false).unwrap();
        ids.push((summary.investigation_id, mode, provider_id, model));
    }

    let page = fixture.store.specialist_task_activity(None).unwrap();
    assert_eq!(page.items.len(), 2);
    for (id, mode, provider_id, model) in ids {
        let item = page
            .items
            .iter()
            .find(|item| item.summary.investigation_id == id)
            .unwrap();
        assert_eq!(item.summary.provider_mode, mode);
        assert_eq!(item.saved_provider.mode, mode);
        assert_eq!(item.saved_provider.provider_id, provider_id);
        assert_eq!(item.saved_provider.model, model);
        assert_eq!(
            item.reported_cache_read_tokens.as_ref().unwrap().as_deref(),
            (mode == InvestigationProviderMode::Local).then_some("18446744073709551615")
        );
        let usage = item.reported_usage.as_ref().unwrap();
        assert_eq!(
            usage.input_tokens.as_deref(),
            (mode == InvestigationProviderMode::Local).then_some("18446744073709551615")
        );
        assert_eq!(
            usage.output_tokens.as_deref(),
            (mode == InvestigationProviderMode::Cloud).then_some("0")
        );
    }
    let wire = serde_json::to_string(&page).unwrap();
    assert!(!wire.contains("127.0.0.1"));
    assert!(!wire.contains("provider.invalid"));
    let json = serde_json::to_value(&page).unwrap();
    for item in json["items"].as_array().unwrap() {
        let provider = item["saved_provider"].as_object().unwrap();
        assert!(provider.contains_key("mode"));
        assert!(provider.contains_key("provider_id"));
        assert!(provider.contains_key("model"));
        assert!(!provider.contains_key("endpoint"));
        assert!(!provider.contains_key("deployment"));
    }
    assert!(wire.len() <= 256 * 1024);
}

#[test]
fn investigation_specialist_activity_reuses_active_selection_order_and_rejects_invalid_cursor() {
    // AC-0381: the projection uses the existing bounded active selection.
    let mut fixture = Fixture::new();
    for index in 0..MAX_LIVE_TASKS {
        fixture
            .store
            .start_investigation(
                &request(
                    &format!("provider-cursor:{index}"),
                    InvestigationProviderMode::Local,
                ),
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap();
    }

    let expected = fixture
        .store
        .investigation_history_selection(None, true, None)
        .unwrap();
    let first = fixture.store.specialist_task_activity(None).unwrap();
    assert_eq!(first.items.len(), expected.items.len());
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.summary.investigation_id.as_str())
            .collect::<Vec<_>>(),
        expected
            .items
            .iter()
            .map(|item| item.investigation_id.as_str())
            .collect::<Vec<_>>(),
    );
    assert_eq!(first.next_cursor, expected.next_cursor);
    assert_eq!(first.items.len(), MAX_LIVE_TASKS);
    assert!(first.next_cursor.is_none());
    assert!(first.items.iter().all(|item| item.reported_usage.is_some()));
    assert!(serde_json::to_vec(&first).unwrap().len() <= 256 * 1024);
    assert!(
        fixture
            .store
            .specialist_task_activity(Some("not-a-cursor"))
            .is_err()
    );
}

#[test]
fn investigation_specialist_activity_usage_fits_exactly_or_keeps_legacy_base_bytes() {
    // AC-0417: optional telemetry never invalidates an otherwise valid legacy item.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("usage-boundary", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    let id = detail.summary.investigation_id;
    let mut task = load_task(&fixture.store.conn, &id).unwrap();
    let mut item = fixture
        .store
        .specialist_task_activity(None)
        .unwrap()
        .items
        .remove(0);
    let mut growth = 4096 - serde_json::to_vec(&item).unwrap().len();
    let extra = growth.min(2048 - item.saved_provider.provider_id.len());
    item.saved_provider.provider_id.push_str(&"p".repeat(extra));
    growth -= extra;
    assert!(item.saved_provider.model.len() + growth < 2048);
    item.saved_provider.model.push_str(&"m".repeat(growth));
    task.detail.provider.provider_id = item.saved_provider.provider_id;
    task.detail.provider.model = item.saved_provider.model;
    storage::save_task(
        &fixture.store.conn,
        &task,
        task.detail.summary.revision,
        false,
    )
    .unwrap();
    let exact = fixture.store.specialist_task_activity(None).unwrap();
    assert!(exact.items[0].reported_usage.is_some());
    assert_eq!(serde_json::to_vec(&exact.items[0]).unwrap().len(), 4096);
    assert!(exact.items[0].reported_cache_read_tokens.is_some());
    task.detail.provider.model.push('m');
    storage::save_task(
        &fixture.store.conn,
        &task,
        task.detail.summary.revision,
        false,
    )
    .unwrap();
    let cache_omitted = fixture.store.specialist_task_activity(None).unwrap();
    assert!(cache_omitted.items[0].reported_cache_read_tokens.is_none());
    assert!(cache_omitted.items[0].reported_usage.is_some());
    let mut without_cache = exact.items[0].clone();
    without_cache.reported_cache_read_tokens = None;
    without_cache.saved_provider.model.push('m');
    assert_eq!(
        serde_json::to_vec(&cache_omitted.items[0]).unwrap(),
        serde_json::to_vec(&without_cache).unwrap()
    );
    // Consume the room recovered by omitting cache, then cross the old boundary.
    let extra = 4097 - serde_json::to_vec(&cache_omitted.items[0]).unwrap().len();
    task.detail.provider.model.push_str(&"m".repeat(extra));
    storage::save_task(
        &fixture.store.conn,
        &task,
        task.detail.summary.revision,
        false,
    )
    .unwrap();
    let legacy =
        reads::encode_specialist_activity_item(&task.detail.summary, &task.detail.provider)
            .unwrap();
    let page = fixture.store.specialist_task_activity(None).unwrap();
    assert!(page.items[0].reported_usage.is_none());
    assert_eq!(serde_json::to_vec(&page.items[0]).unwrap(), legacy);
    assert!(page.next_cursor.is_none());
    assert_eq!(
        fixture
            .store
            .investigation_history(None, None)
            .unwrap()
            .items[0],
        task.detail.summary
    );
}

#[test]
fn investigation_specialist_activity_rejects_malformed_and_oversized_identity() {
    // AC-0381: invalid saved mode bindings and over-budget projected items fail closed.
    let mut malformed_fixture = Fixture::new();
    let (malformed_id, _) =
        malformed_fixture.start("provider-mode-mismatch", InvestigationProviderMode::Local);
    let mut malformed = load_task(&malformed_fixture.store.conn, &malformed_id).unwrap();
    malformed.detail.summary.provider_mode = InvestigationProviderMode::Cloud;
    let bytes = storage::encode(&malformed, MAX_RECORD_BYTES).unwrap();
    let record_hash = core_prov::content_hash(&bytes);
    malformed_fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
            rusqlite::params![bytes, record_hash, malformed_id],
        )
        .unwrap();
    assert!(matches!(
        malformed_fixture.store.specialist_task_activity(None),
        Err(InvestigationStoreError::Invalid),
    ));

    // Existing tasks can predate this projection admission check. Ordinary
    // history remains readable while the app-only activity feed fails closed.
    let mut legacy_fixture = Fixture::new();
    let (legacy_id, _) =
        legacy_fixture.start("legacy-large-provider", InvestigationProviderMode::Local);
    let mut legacy = load_task(&legacy_fixture.store.conn, &legacy_id).unwrap();
    legacy.detail.provider.provider_id = "p".repeat(2048);
    legacy.detail.provider.model = "m".repeat(2048);
    let bytes = storage::encode(&legacy, MAX_RECORD_BYTES).unwrap();
    let record_hash = core_prov::content_hash(&bytes);
    legacy_fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
            rusqlite::params![bytes, record_hash, legacy_id],
        )
        .unwrap();
    assert!(
        legacy_fixture
            .store
            .investigation_history(None, None)
            .is_ok()
    );
    assert!(matches!(
        legacy_fixture.store.specialist_task_activity(None),
        Err(InvestigationStoreError::Capacity),
    ));
}

#[test]
fn investigation_specialist_activity_admission_charges_exact_escaped_projection() {
    let mut fixture = Fixture::new();
    let mut saved = provider(InvestigationProviderMode::Local);
    saved.provider_id = "provider-quote\"-slash\\-雪".into();
    saved.model = "model-λ\\\"".into();
    let detail = fixture
        .store
        .start_investigation(
            &request(
                "escaped-provider-identity",
                InvestigationProviderMode::Local,
            ),
            Some(&saved),
        )
        .unwrap()
        .detail;
    let mut item = fixture
        .store
        .specialist_task_activity(None)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.summary.investigation_id == detail.summary.investigation_id)
        .unwrap();
    let task = load_task(&fixture.store.conn, &detail.summary.investigation_id).unwrap();
    let exact_projection =
        reads::encode_specialist_activity_item(&item.summary, &task.detail.provider).unwrap();
    let current = serde_json::to_value(&item).unwrap();
    assert!(current["reported_usage"]["input_tokens"].is_null());
    item.reported_usage = None;
    item.reported_cache_read_tokens = None;
    assert_eq!(exact_projection, serde_json::to_vec(&item).unwrap());
    assert!(exact_projection.windows(2).any(|bytes| bytes == b"\\\""));
    assert!(
        std::str::from_utf8(&exact_projection)
            .unwrap()
            .contains("雪")
    );
}

#[test]
fn investigation_specialist_activity_admission_reserves_mutable_growth_through_transitions() {
    let mut probe = Fixture::new();
    let mut saved = provider(InvestigationProviderMode::Local);
    saved.provider_id = "p".into();
    saved.model = "m".into();
    let probe_detail = probe
        .store
        .start_investigation(
            &request(
                "provider-item-sizing-probe",
                InvestigationProviderMode::Local,
            ),
            Some(&saved),
        )
        .unwrap()
        .detail;
    let probe_record =
        load_task(&probe.store.conn, &probe_detail.summary.investigation_id).unwrap();
    let probe_size = reads::specialist_activity_admission_bytes(&probe_record)
        .unwrap()
        .len();
    assert!(probe_size < 4096);
    let mut growth = 4096 - probe_size;
    let provider_growth = growth.min(2048 - saved.provider_id.len());
    saved.provider_id.push_str(&"p".repeat(provider_growth));
    growth -= provider_growth;
    let model_growth = growth.min(2048 - saved.model.len());
    saved.model.push_str(&"m".repeat(model_growth));
    growth -= model_growth;
    assert_eq!(
        growth, 0,
        "existing provider limits cannot approach the item bound"
    );
    let mut boundary_record = probe_record.clone();
    boundary_record.detail.provider = saved.clone();
    assert_eq!(
        reads::specialist_activity_admission_bytes(&boundary_record)
            .unwrap()
            .len(),
        4096,
    );

    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("provider-item-near-limit", InvestigationProviderMode::Local),
            Some(&saved),
        )
        .unwrap()
        .detail;
    let id = detail.summary.investigation_id;
    let stored = load_task(&fixture.store.conn, &id).unwrap();
    let reserved = reads::specialist_activity_admission_bytes(&stored)
        .unwrap()
        .len();
    assert_eq!(reserved, 4096);

    let page = fixture.store.specialist_task_activity(None).unwrap();
    assert_eq!(page.items[0].summary.status, InvestigationStatus::Queued);
    let actual = serde_json::to_vec(&page.items[0]).unwrap();
    assert!(actual.len() <= reserved);
    let plan = fixture
        .store
        .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = fixture.locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = fixture.store.claim_execution(&plan, reservation).unwrap();
    let preparing = fixture.store.specialist_task_activity(None).unwrap();
    assert_eq!(
        preparing.items[0].summary.status,
        InvestigationStatus::Preparing,
    );
    assert!(serde_json::to_vec(&preparing.items[0]).unwrap().len() <= reserved);
    let ledger = fixture.prepare(&id, &execution, true);
    begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let running = fixture.store.specialist_task_activity(None).unwrap();
    assert_eq!(
        running.items[0].summary.status,
        InvestigationStatus::Running,
    );
    assert!(serde_json::to_vec(&running.items[0]).unwrap().len() <= reserved);
    let cancelled = fixture.store.cancel_investigation(&id).unwrap();
    assert!(cancelled.summary.cancel_requested);
    let requested = fixture.store.specialist_task_activity(None).unwrap();
    assert!(requested.items[0].summary.cancel_requested);
    assert!(serde_json::to_vec(&requested.items[0]).unwrap().len() <= reserved);

    // One more ASCII identity byte crosses the exact preflight boundary.
    let mut over = saved;
    if over.provider_id.len() < 2048 {
        over.provider_id.push('p');
    } else {
        over.model.push('m');
    }
    let mut rejected = Fixture::new();
    assert!(matches!(
        rejected.store.start_investigation(
            &request("provider-item-over-limit", InvestigationProviderMode::Local),
            Some(&over),
        ),
        Err(InvestigationStoreError::Capacity),
    ));
    for table in [
        "investigation_tasks",
        "investigation_conversations",
        "jobs",
        "job_attempts",
    ] {
        let count: i64 = rejected
            .store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "oversized activity admission left rows in {table}"
        );
    }
}

#[test]
fn investigation_specialist_activity_capacity_rejection_does_not_change_nonce_replay() {
    let mut fixture = Fixture::new();
    let request = request("activity-capacity-replay", InvestigationProviderMode::Local);
    let first = fixture
        .store
        .start_investigation(&request, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap();
    let mut excessive = provider(InvestigationProviderMode::Local);
    excessive.provider_id = "p".repeat(2048);
    excessive.model = "m".repeat(2048);
    let replay = fixture
        .store
        .start_investigation(&request, Some(&excessive))
        .unwrap();
    assert!(first.created);
    assert!(!replay.created);
    assert_eq!(
        first.detail.summary.investigation_id,
        replay.detail.summary.investigation_id,
    );
}

#[test]
fn investigation_active_history_keeps_cancellation_requested_until_terminal() {
    // AC-0372: cancellation is a request while an invocation remains owned.
    let mut fixture = Fixture::new();
    let (id, execution) =
        fixture.start("active-cancel-requested", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, true);
    let running = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let requested = fixture.store.cancel_investigation(&id).unwrap();
    assert_eq!(requested.summary.status, InvestigationStatus::Running);
    assert!(requested.summary.cancel_requested);
    assert_eq!(
        fixture
            .store
            .investigation_history_selection(None, true, None)
            .unwrap()
            .items
            .iter()
            .find(|item| item.investigation_id == id)
            .unwrap()
            .status,
        running.summary.status
    );
}

#[test]
fn investigation_active_history_status_set_excludes_interrupted_and_unknown() {
    // AC-0372: only executing/consent-waiting states are active; uncertain
    // and interrupted durable outcomes remain historical, not running work.
    for status in [
        InvestigationStatus::Queued,
        InvestigationStatus::Preparing,
        InvestigationStatus::Running,
        InvestigationStatus::AwaitingConsent,
    ] {
        assert!(super::reads::investigation_status_is_active(status));
    }
    for status in [
        InvestigationStatus::Completed,
        InvestigationStatus::Failed,
        InvestigationStatus::Cancelled,
        InvestigationStatus::Interrupted,
        InvestigationStatus::OutcomeUnknown,
    ] {
        assert!(!super::reads::investigation_status_is_active(status));
    }
}

#[test]
fn investigation_strict_storage_rejects_oversize_records_and_partial_schemas() {
    // AC-0188: SQL rejects oversized bodies before retrieving them; private
    // schema migration never CREATE-repairs a partial or future owned schema.
    let mut fixture = Fixture::new();
    let detail = fixture
        .store
        .start_investigation(
            &request("corrupt", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail;
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=zeroblob(262145) WHERE id=?1",
            [&detail.summary.investigation_id],
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation(&detail.summary.investigation_id)
            .unwrap_err(),
        InvestigationStoreError::Invalid
    );
    for sql in [
        "CREATE TABLE investigation_meta(future TEXT)",
        "CREATE VIEW investigation_tasks AS SELECT 1 AS id",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.db");
        Connection::open(&path).unwrap().execute(sql, []).unwrap();
        assert!(JobStore::open(&path).is_err());
    }
}

#[test]
fn investigation_unknown_usage_stays_unknown_and_stale_decisions_do_not_write() {
    // AC-0185/0186: later measured responses cannot turn an earlier unknown
    // invocation into a complete measured total; stale revisions cause no writes.
    assert_eq!(
        transitions::add_reported(None, Some(7), true).unwrap(),
        Some(7)
    );
    assert_eq!(
        transitions::add_reported(None, Some(7), false).unwrap(),
        None
    );
    assert_eq!(
        transitions::add_reported(Some(5), None, false).unwrap(),
        None
    );
    assert_eq!(
        transitions::add_reported(Some(5), Some(7), false).unwrap(),
        Some(12)
    );
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("stale", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    let before = fixture.store.investigation(&id).unwrap();
    assert_eq!(
        fixture
            .store
            .advance_investigation(
                &execution,
                before.summary.revision - 1,
                InvestigationTransition::BeginInvocation {
                    step: step(&ledger, 1)
                }
            )
            .unwrap_err(),
        InvestigationStoreError::Stale
    );
    assert_eq!(fixture.store.investigation(&id).unwrap(), before);
    assert!(
        fixture
            .store
            .investigation_timestamp(0)
            .unwrap()
            .ends_with('Z')
    );
    assert!(fixture.store.investigation_timestamp(3601).is_err());
}

#[test]
fn investigation_receipt_index_detects_missing_and_relabelled_entries_before_filtering() {
    // AC-0188/0189: canonical triggers are insufficient to certify data after
    // corruption. Reconcile the complete immutable ledger union before filtering.
    for remove in [true, false] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("corrupt-index", InvestigationProviderMode::Local);
        let ledger = fixture.prepare(&id, &execution, true);
        let source = &ledger.receipt_references[0].source_id;
        let trigger = if remove {
            "investigation_refs_no_delete"
        } else {
            "investigation_refs_no_update"
        };
        let sql: String = fixture
            .store
            .conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name=?1",
                [trigger],
                |row| row.get(0),
            )
            .unwrap();
        fixture
            .store
            .conn
            .execute_batch(&format!("DROP TRIGGER {trigger}"))
            .unwrap();
        if remove {
            fixture
                .store
                .conn
                .execute("DELETE FROM investigation_refs WHERE task_id=?1", [&id])
                .unwrap();
        } else {
            fixture.store.conn.execute(
                "UPDATE investigation_refs SET source_id='src_fedcba9876543210fedcba9876543210' WHERE task_id=?1", [&id],
            ).unwrap();
        }
        fixture.store.conn.execute_batch(&sql).unwrap();
        for requested in [source.as_str(), "src_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"] {
            assert_eq!(
                fixture
                    .store
                    .investigation_receipt_references(requested)
                    .unwrap_err(),
                InvestigationStoreError::Invalid
            );
        }
        // The authoritative input remains intact and independently readable.
        assert_eq!(
            fixture.store.investigation_ledger(&id).unwrap(),
            Some(ledger)
        );
    }
}

#[test]
fn investigation_failed_invocation_retains_elapsed_and_marks_aggregate_usage_unknown() {
    // AC-0185/0186/0187: a known failed call and an uncertain call retain actual
    // elapsed time, including cooperative overshoot, without inventing token use.
    for reason in [
        InvestigationStopReason::ProviderFailure,
        InvestigationStopReason::OutcomeUnknown,
    ] {
        let mut fixture = Fixture::new();
        let (id, execution) = fixture.start("failed-metrics", InvestigationProviderMode::Local);
        let mut ledger = fixture.prepare(&id, &execution, false);
        let started = begin(&mut fixture, &id, &execution, step(&ledger, 1));
        let mut measured = response("step:1", 100);
        measured.reported_input_tokens = Some(120);
        measured.reported_output_tokens = Some(24);
        measured.reported_cache_read_tokens = Some(7);
        let action = fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::ActionAdmitted {
                    response: measured,
                    tool: Some(InvestigationTool::QueryContext),
                    result: None,
                },
            )
            .unwrap();
        let tool = fixture
            .store
            .advance_investigation(
                &execution,
                action.summary.revision,
                InvestigationTransition::BeginTool {
                    tool: InvestigationTool::QueryContext,
                },
            )
            .unwrap();
        ledger.revision += 1;
        fixture
            .store
            .advance_investigation(
                &execution,
                tool.summary.revision,
                InvestigationTransition::InputAdmitted {
                    ledger: Box::new(ledger.clone()),
                    usage: tool.usage,
                },
            )
            .unwrap();
        let pending = begin(&mut fixture, &id, &execution, step(&ledger, 2));
        assert_eq!(pending.usage.reported_input_tokens, Some(120));
        assert_eq!(pending.usage.reported_cache_read_tokens, Some(7));
        assert_eq!(
            fixture
                .store
                .advance_investigation(
                    &execution,
                    pending.summary.revision,
                    InvestigationTransition::StopWithElapsed {
                        reason,
                        active_milliseconds: 99
                    }
                )
                .unwrap_err(),
            InvestigationStoreError::Invalid
        );
        assert_eq!(fixture.store.investigation(&id).unwrap(), pending);
        let stopped = fixture
            .store
            .advance_investigation(
                &execution,
                pending.summary.revision,
                InvestigationTransition::StopWithElapsed {
                    reason,
                    active_milliseconds: 600_123,
                },
            )
            .unwrap();
        assert_eq!(stopped.usage.active_milliseconds, 600_123);
        assert_eq!(stopped.usage.reported_input_tokens, None);
        assert_eq!(stopped.usage.reported_output_tokens, None);
        assert_eq!(stopped.usage.reported_cache_read_tokens, None);
        assert_eq!(stopped.usage.model_invocations, 2);
        assert_eq!(
            stopped.summary.status,
            if reason == InvestigationStopReason::OutcomeUnknown {
                InvestigationStatus::OutcomeUnknown
            } else {
                InvestigationStatus::Failed
            }
        );
        drop(fixture.store);
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert_eq!(reopened.investigation(&id).unwrap(), stopped);
    }
}

#[test]
fn investigation_compact_disclosure_is_durable_and_finishing_binding_is_revalidated() {
    // AC-0234/0235: persist exact disclosure before execution; reopen never
    // relies on current source or merely the cumulative citation membership.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start_as(
        "compact",
        InvestigationProviderMode::Local,
        SpecialistId::DomainAnalystV3,
    );
    let ledger = fixture.prepare(&id, &execution, false);
    let mut pending = step(&ledger, 1);
    let current = fixture.store.investigation(&id).unwrap();
    assert_eq!(
        fixture
            .store
            .advance_investigation(
                &execution,
                current.summary.revision,
                InvestigationTransition::BeginInvocation {
                    step: pending.clone()
                }
            )
            .unwrap_err(),
        InvestigationStoreError::Invalid
    );
    let supplied = || DisclosureRepresentation::Supplied {
        representation_hash: hash("representation"),
    };
    let disclosure = InvestigationDisclosure {
        admission: None,
        query_history: None,
        completion_contract_hash: None,
        completion_metadata_hash: None,
        coverage_details: vec![],
        coverage: None,
        inspection_requirements_hash: None,
        schema_version: 1,
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: pending.payload_hash.clone(),
        facts: ledger
            .selected_facts
            .iter()
            .map(|selected| DisclosedInvestigationFact {
                fact: selected.fact.clone(),
                representation: supplied(),
            })
            .collect(),
        queries: ledger
            .queries
            .iter()
            .enumerate()
            .map(|(query_index, _)| DisclosedInvestigationQuery {
                query_index,
                representation: supplied(),
            })
            .collect(),
        sources: vec![],
        history: None,
    };
    pending.disclosure = Some(disclosure.clone());
    let mut mismatched = pending.clone();
    mismatched.payload_hash = hash("wrong-payload");
    assert!(validate_step(&mismatched, &InvestigationLimits::default()).is_err());
    begin(&mut fixture, &id, &execution, pending.clone());
    let saved: PendingInvestigationStep = read_body(
        &fixture.store.conn,
        "investigation_invocations",
        "step_id",
        &id,
        &pending.step_id,
    )
    .unwrap()
    .unwrap();
    assert_eq!(saved, pending);
    let result = InvestigationResult::admit_with_disclosure(
        &id,
        &ledger,
        &pending.step_id,
        &disclosure,
        &InvestigationAction::Finish {
            findings: vec![],
            knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
            limitations: vec!["Only bounded metadata was inspected.".into()],
        },
        std::iter::empty(),
        Some("fixture-model".into()),
        "2026-09-11T00:00:00Z".into(),
    )
    .unwrap();
    let current = fixture.store.investigation(&id).unwrap();
    fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response(&pending.step_id, 100),
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_result(&id).unwrap(),
        Some(result.clone())
    );
    // Valid body/hash tampering still cannot remove the finishing disclosure.
    let mut missing = pending;
    missing.disclosure = None;
    let body = encode(&missing, MAX_INVOCATION_BYTES).unwrap();
    assert!(
        validate_result_basis(
            &result,
            &ledger,
            &missing,
            &InvestigationLimits::default(),
            None
        )
        .is_err()
    );
    assert!(
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_invocations SET body=?1, body_hash=?2 WHERE task_id=?3",
                params![body, core_prov::content_hash(&body), id],
            )
            .is_err()
    );
    assert_eq!(reopened.investigation_result(&id).unwrap(), Some(result));
}

#[test]
fn investigation_legacy_step_serialization_omits_disclosure() {
    let pending = step(&ledger(false), 1);
    let body = encode(&pending, 4096).unwrap();
    let literal = format!(
        "{{\"step_id\":\"step:1\",\"payload_hash\":\"{}\",\"input_ledger_hash\":\"{}\",\"input_bytes\":1024,\"generated_tokens\":2048}}",
        pending.payload_hash, pending.input_ledger_hash
    );
    assert_eq!(body, literal.as_bytes());
    let decoded: PendingInvestigationStep =
        decode_record(std::str::from_utf8(&body).unwrap()).unwrap();
    assert_eq!(encode(&decoded, 4096).unwrap(), body);
}

fn compact_step(ledger: &InvestigationInputLedger) -> PendingInvestigationStep {
    let mut pending = step(ledger, 1);
    let supplied = || DisclosureRepresentation::Supplied {
        representation_hash: hash("representation"),
    };
    pending.disclosure = Some(InvestigationDisclosure {
        admission: None,
        query_history: None,
        completion_contract_hash: None,
        completion_metadata_hash: None,
        coverage_details: vec![],
        coverage: None,
        inspection_requirements_hash: None,
        schema_version: 1,
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: pending.payload_hash.clone(),
        facts: ledger
            .selected_facts
            .iter()
            .map(|selected| DisclosedInvestigationFact {
                fact: selected.fact.clone(),
                representation: supplied(),
            })
            .collect(),
        queries: ledger
            .queries
            .iter()
            .enumerate()
            .map(|(query_index, _)| DisclosedInvestigationQuery {
                query_index,
                representation: supplied(),
            })
            .collect(),
        sources: vec![],
        history: None,
    });
    pending
}

#[test]
fn investigation_compact_storage_failure_before_dispatch_reserves_nothing() {
    // AC-0234/0188: failure at the invocation write boundary cannot authorize
    // a call or consume budgets; the full state reopens unchanged.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start_as(
        "write-failure",
        InvestigationProviderMode::Local,
        SpecialistId::DomainAnalystV3,
    );
    let ledger = fixture.prepare(&id, &execution, false);
    let before = fixture.store.investigation(&id).unwrap();
    fixture
        .store
        .conn
        .pragma_update(None, "query_only", true)
        .unwrap();
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                before.summary.revision,
                InvestigationTransition::BeginInvocation {
                    step: compact_step(&ledger)
                }
            )
            .is_err()
    );
    fixture
        .store
        .conn
        .pragma_update(None, "query_only", false)
        .unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), before);
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_invocations WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(before.usage.model_invocations, 0);
    assert_eq!(before.usage.generated_token_reservations, 0);
}

#[test]
fn investigation_compact_consent_binds_exact_manifest_across_reopen() {
    // AC-0234/0186: matching payload alone is insufficient when a different
    // disclosure is substituted after approval; the original starts only once.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start_as(
        "compact-cloud",
        InvestigationProviderMode::Cloud,
        SpecialistId::DomainAnalystV3,
    );
    let ledger = fixture.prepare(&id, &execution, false);
    let pending = compact_step(&ledger);
    let current = fixture.store.investigation(&id).unwrap();
    let waiting = fixture
        .store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::AwaitConsent {
                step: pending.clone(),
            },
        )
        .unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        reopened.investigation_pending_step(&id).unwrap(),
        Some((waiting.summary.revision, pending.clone(), false))
    );
    let approved = fixture
        .store
        .approve_investigation(
            &id,
            waiting.summary.revision,
            &pending.step_id,
            &pending.payload_hash,
        )
        .unwrap();
    let mut substituted = pending.clone();
    substituted.disclosure.as_mut().unwrap().facts[0].representation =
        DisclosureRepresentation::Omitted {
            reason: DisclosureOmissionReason::NotSelected,
        };
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                approved.summary.revision,
                InvestigationTransition::BeginInvocation { step: substituted }
            )
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .investigation(&id)
            .unwrap()
            .usage
            .model_invocations,
        0
    );
    let started = fixture
        .store
        .advance_investigation(
            &execution,
            approved.summary.revision,
            InvestigationTransition::BeginInvocation {
                step: pending.clone(),
            },
        )
        .unwrap();
    assert_eq!(started.usage.model_invocations, 1);
    assert!(
        fixture
            .store
            .advance_investigation(
                &execution,
                started.summary.revision,
                InvestigationTransition::BeginInvocation { step: pending }
            )
            .is_err()
    );
}

#[path = "policy_tests.rs"]
mod policy_tests;

#[path = "input_storage_helper_tests.rs"]
mod input_storage_helper_tests;

#[test]
fn finish_only_tool_stop_preserves_call_charge_without_tool_or_input_publication() {
    // AC-0287: durable stop primitive used by the new-family worker guard.
    // This does not activate @10 or prove its saved-call/journal binding.
    let mut fixture = Fixture::new();
    let (id, execution) = fixture.start("finish-only-stop", InvestigationProviderMode::Local);
    let ledger = fixture.prepare(&id, &execution, false);
    let pending = begin(&mut fixture, &id, &execution, step(&ledger, 1));
    let stopped = fixture
        .store
        .advance_investigation(
            &execution,
            pending.summary.revision,
            InvestigationTransition::StopWithElapsed {
                reason: InvestigationStopReason::FinishOnlyToolProposal,
                active_milliseconds: pending.usage.active_milliseconds,
            },
        )
        .unwrap();
    assert_eq!(stopped.summary.status, InvestigationStatus::Failed);
    assert_eq!(
        stopped.usage.model_invocations,
        pending.usage.model_invocations
    );
    assert_eq!(
        stopped.usage.generated_token_reservations,
        pending.usage.generated_token_reservations
    );
    assert_eq!(stopped.usage.tool_actions, 0);
    assert_eq!(stopped.usage.evidence_requests, 0);
    assert_eq!(
        fixture.store.investigation_ledger(&id).unwrap(),
        Some(ledger)
    );
    assert_eq!(fixture.store.investigation(&id).unwrap(), stopped);
    assert_eq!(
        stopped.error.as_deref(),
        Some(InvestigationStopReason::FinishOnlyToolProposal.message())
    );
}

#[path = "finding_review_tests.rs"]
mod finding_review_tests;

#[path = "namespace_migration_tests.rs"]
mod namespace_migration_tests;

#[path = "origin_namespace_tests.rs"]
mod origin_namespace_tests;

#[path = "principal_access_tests.rs"]
mod principal_access_tests;

#[path = "mcp_nonce_tests.rs"]
mod mcp_nonce_tests;

#[path = "mcp_live_cap_tests.rs"]
mod mcp_live_cap_tests;

#[test]
fn investigation_finish_diagnostic_classes_fit_the_existing_message_bound() {
    // AC-0184 (#937): finite internal classes do not widen persisted strings.
    use agents::investigation::InvestigationError::*;
    let errors = [
        InvalidRequest,
        InvalidAction,
        LimitExceeded,
        InvalidInput,
        InvalidCitation,
        SourceReplay,
        SensitiveOutput,
    ];
    let messages: std::collections::BTreeSet<_> = errors
        .into_iter()
        .map(|error| InvestigationStopReason::InvalidFinish(error).message())
        .collect();
    assert_eq!(messages.len(), errors.len());
    for message in messages
        .into_iter()
        .chain([InvestigationStopReason::UnadmittedRead.message()])
    {
        assert!(message.len() <= InvestigationStopReason::InvalidResponse.message().len());
    }
}

#[path = "source_replay_tests.rs"]
mod source_replay_tests;

// AC-0402: validated answer-only hits participate in the same bounded high-water pages.
#[test]
fn investigation_history_search_answers_and_corruption() {
    let mut f = Fixture::new();
    let (id, execution) = f.start("answer-search", InvestigationProviderMode::Local);
    let ledger = f.prepare(&id, &execution, false);
    let started = begin(&mut f, &id, &execution, step(&ledger, 1));
    f.store
        .advance_investigation(
            &execution,
            started.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: response("step:1", 1),
                tool: None,
                result: Some(Box::new(finish(&id, &ledger))),
            },
        )
        .unwrap();
    let named = f
        .store
        .search_investigation_history("Domain analyst", None)
        .unwrap();
    assert_eq!(named.name_match_ids, std::slice::from_ref(&id));
    assert!(named.items[0].has_result && named.answer_match_ids.is_empty());
    for index in 0..51 {
        let mut requested = request(
            &format!("answer-search:{index}"),
            InvestigationProviderMode::Local,
        );
        requested.question = format!("Bounded support metadata {index}");
        let detail = f
            .store
            .start_investigation(
                &requested,
                Some(&provider(InvestigationProviderMode::Local)),
            )
            .unwrap()
            .detail;
        f.store
            .cancel_investigation(&detail.summary.investigation_id)
            .unwrap();
    }
    let ordinary = f.store.investigation_history(None, None).unwrap();
    assert!(!ordinary.items.iter().any(|row| row.investigation_id == id));
    let wire = serde_json::to_value(&ordinary).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 2);
    assert!(wire.get("answer_match_ids").is_none());
    let first = f
        .store
        .search_investigation_history("  BOUNDED SUPPORT  ", None)
        .unwrap();
    assert_eq!(first.items.len(), 50);
    assert!(first.answer_match_ids.is_empty());
    let cursor = first.next_cursor.unwrap();
    let mut newer = request("answer-search-new", InvestigationProviderMode::Local);
    newer.question = "Bounded support newer than high water".into();
    let newer_id = f
        .store
        .start_investigation(&newer, Some(&provider(InvestigationProviderMode::Local)))
        .unwrap()
        .detail
        .summary
        .investigation_id;
    f.store.cancel_investigation(&newer_id).unwrap();
    let second = f
        .store
        .search_investigation_history("bounded support", Some(&cursor))
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert_eq!(second.answer_match_ids, vec![id.clone()]);
    assert!(
        !second
            .items
            .iter()
            .any(|row| row.investigation_id == newer_id)
    );
    let statement = f
        .store
        .search_investigation_history("RESPONSIBILITY WORTH", None)
        .unwrap();
    assert_eq!(statement.answer_match_ids, vec![id.clone()]);
    assert!(
        !serde_json::to_string(&statement)
            .unwrap()
            .contains("worth examining")
    );
    assert!(
        f.store
            .search_investigation_history("responsibility%", None)
            .unwrap()
            .items
            .is_empty()
    );
    let continuation = f
        .store
        .investigation_history(None, ordinary.next_cursor.as_deref())
        .unwrap();
    assert!(
        continuation
            .items
            .iter()
            .any(|item| item.investigation_id == id)
    );
    assert!(continuation.answer_match_ids.is_empty());
    let trigger: String = f
        .store
        .conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name='investigation_results_no_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    f.store
        .conn
        .execute_batch("DROP TRIGGER investigation_results_no_update")
        .unwrap();
    f.store
        .conn
        .execute(
            "UPDATE investigation_results SET body_hash='invalid' WHERE task_id=?1",
            [&id],
        )
        .unwrap();
    f.store.conn.execute_batch(&trigger).unwrap();
    assert!(
        f.store
            .search_investigation_history("responsibility", None)
            .is_err()
    );
    let name_cursor = f
        .store
        .search_investigation_history("Domain analyst", None)
        .unwrap()
        .next_cursor
        .expect("the corrupt older task is beyond the first name page");
    assert!(
        f.store
            .search_investigation_history("Domain analyst", Some(&name_cursor))
            .is_err()
    );
    // Existing task validation already rejects corrupt saved results, even without answer matching.
    assert!(
        f.store
            .search_investigation_history("What evidence supports", None)
            .is_err()
    );
    assert!(
        f.store
            .investigation_history(None, ordinary.next_cursor.as_deref())
            .is_err()
    );
}

#[test]
fn investigation_start_diagnostics_identify_contention_without_partial_rows() {
    // AC-0181/0188: actual SQLite failure, with the unchanged one-second timeout.
    let mut fixture = Fixture::new();
    let mut blocker = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let held = blocker
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (result, diagnostic) = start_diagnostics::capture(|| {
        fixture.store.start_investigation(
            &request("held-writer", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
    });
    assert_eq!(result.unwrap_err(), InvestigationStoreError::Storage);
    assert_eq!(diagnostic.stage, "begin_immediate");
    assert_eq!(diagnostic.sqlite_codes, Some((5, 5)));
    for table in [
        "jobs",
        "investigation_tasks",
        "investigation_conversations",
        "investigation_chats",
        "investigation_chat_messages",
    ] {
        let count: i64 = fixture
            .store
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    held.rollback().unwrap();
    assert!(
        fixture
            .store
            .start_investigation(
                &request("held-writer", InvestigationProviderMode::Local),
                Some(&provider(InvestigationProviderMode::Local))
            )
            .unwrap()
            .created
    );
}

#[test]
fn investigation_start_diagnostics_are_opt_in_private_and_unwind_scoped() {
    // AC-0188: neither raw SQLite text nor thread state escapes the opt-in scope.
    let private = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(5),
        Some("PRIVATE path/payload".into()),
    );
    let (_, private_snapshot) = start_diagnostics::capture(|| {
        start_diagnostics::stage("conversion");
        let _ = InvestigationStoreError::from(private);
    });
    assert_eq!(private_snapshot.sqlite_codes, Some((5, 5)));
    assert!(!format!("{private_snapshot:?}").contains("PRIVATE"));
    let _ = InvestigationStoreError::from(rusqlite::Error::InvalidQuery);
    let (_, outer) = start_diagnostics::capture(|| {
        start_diagnostics::stage("outer");
        let _ = std::panic::catch_unwind(|| {
            start_diagnostics::capture(|| {
                start_diagnostics::stage("inner");
                panic!("synthetic unwind");
            })
        });
    });
    assert_eq!(outer.stage, "outer");
    assert_eq!(outer.sqlite_codes, None);
    let (_, fresh) = start_diagnostics::capture(|| {});
    assert_eq!(fresh, start_diagnostics::Snapshot::default());
}

// AC-0420: app-only counting does not alter canonical detail or its durable owner.
#[test]
fn investigation_detail_read_exact_reports_preserve_base_and_capacity() {
    let mut fixture = Fixture::new();
    let (id, _) = fixture.start("detail-usage", InvestigationProviderMode::Local);
    let mut detail = fixture.store.investigation(&id).unwrap();
    let before: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    let base = encode(&detail, MAX_RECORD_BYTES).unwrap();
    let mut read = reads::InvestigationDetailRead::from_detail(detail.clone()).unwrap();
    assert_eq!(
        context_hub::logical::count(&read.detail).unwrap(),
        base.len()
    );
    assert!(
        read.reported_usage_exact
            .as_ref()
            .unwrap()
            .input_tokens
            .is_none()
    );
    read.reported_usage_exact = None;
    assert_eq!(serde_json::to_vec(&read).unwrap(), base);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(reopened.investigation(&id).unwrap(), detail);
    let after: Vec<u8> = reopened
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    detail.usage.reported_input_tokens = Some(u64::MAX);
    detail.usage.reported_output_tokens = Some(u64::MAX);
    detail.usage.reported_cache_read_tokens = Some(u64::MAX);
    let widest = reads::InvestigationDetailRead::from_detail(detail.clone()).unwrap();
    assert_eq!(
        context_hub::logical::count(&widest).unwrap()
            - context_hub::logical::count(&detail).unwrap(),
        145
    );
    detail.usage.reported_output_tokens = Some(0);
    let read = reads::InvestigationDetailRead::from_detail(detail.clone()).unwrap();
    let json = serde_json::to_value(&read).unwrap();
    assert_eq!(
        json["reported_usage_exact"],
        serde_json::json!({"input_tokens":"18446744073709551615","output_tokens":"0","cache_read_tokens":"18446744073709551615"})
    );
    assert_eq!(
        context_hub::logical::count(&read).unwrap(),
        serde_json::to_vec(&read).unwrap().len()
    );
    // Serializer-only defensive boundary, not a claim this enlarged question
    // satisfies saved-task input validation. All real-family bytes above stay unchanged.
    detail
        .summary
        .question
        .push_str(&"q".repeat(MAX_RECORD_BYTES - context_hub::logical::count(&read).unwrap()));
    let exact = reads::InvestigationDetailRead::from_detail(detail.clone()).unwrap();
    assert_eq!(context_hub::logical::count(&exact), Some(MAX_RECORD_BYTES));
    assert!(exact.reported_usage_exact.is_some());
    detail.summary.question.push('q');
    let fallback = reads::InvestigationDetailRead::from_detail(detail.clone()).unwrap();
    assert!(fallback.reported_usage_exact.is_none());
    assert_eq!(
        serde_json::to_vec(&fallback).unwrap(),
        serde_json::to_vec(&detail).unwrap()
    );
}

#[test]
fn finish_feedback_witness_absence_preserves_response_wire() {
    // AC-0287 / #1695: historical response JSON has no diagnostic field.
    let value = response("step", 7);
    let expected = format!(
        r#"{{"step_id":"step","response_hash":"{}","action_hash":"{}","observed_model":"fixture-model","reported_input_tokens":null,"reported_output_tokens":null,"active_milliseconds":7}}"#,
        hash("response"),
        hash("action"),
    );
    assert_eq!(serde_json::to_string(&value).unwrap(), expected);
    let decoded: InvestigationResponseMeta = serde_json::from_str(&expected).unwrap();
    assert!(decoded.finish_feedback_witness.is_none());
    assert_eq!(decoded, value);
}

#[test]
fn agent_roster_counts_only_in_flight_investigations_per_agent() {
    // AC-1408: the roster reads the active selection; a cancelled run leaves
    // its agent idle, a queued one shows its agent running on its mode.
    let mut fixture = Fixture::new();
    let queued = fixture
        .store
        .start_investigation(
            &request("roster-queued", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    let cancelled = fixture
        .store
        .start_investigation(
            &request("roster-cancelled", InvestigationProviderMode::Local),
            Some(&provider(InvestigationProviderMode::Local)),
        )
        .unwrap()
        .detail
        .summary;
    fixture
        .store
        .cancel_investigation(&cancelled.investigation_id)
        .unwrap();

    let work = fixture.store.active_investigation_work(4).unwrap().unwrap();
    assert_eq!(
        work,
        [(
            queued.specialist_id,
            InvestigationProviderMode::Local,
            InvestigationStatus::Queued
        )]
    );
    assert_eq!(fixture.store.active_investigation_work(0).unwrap(), None);
    let roster = agents::roster::roster_status(agents::roster::roster().unwrap(), &work);
    let analyst = roster
        .iter()
        .find(|row| row.agent.agent == "agt-analyst")
        .unwrap();
    assert_eq!(analyst.state, agents::roster::AgentState::Running);
    assert_eq!((analyst.local_active, analyst.cloud_active), (1, 0));
    assert!(
        roster
            .iter()
            .filter(|row| row.agent.agent != "agt-analyst")
            .all(|row| row.state == agents::roster::AgentState::Idle)
    );
}
