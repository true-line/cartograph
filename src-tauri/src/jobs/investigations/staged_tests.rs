use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use serde_json::json;
use std::time::Duration;

// AC-0265: schema acquisition rejects malformed private journals without
// changing the canonical layout or initializing an absent namespace on read.
#[test]
fn staged_schema_rejects_excess_rows_and_multibyte_sql() {
    let mut connection = Connection::open_in_memory().unwrap();
    assert!(validate_schema(&connection).is_err());
    for case in ["canonical", "missing", "extra", "multibyte"] {
        let tx = connection.transaction().unwrap();
        initialize(&tx).unwrap();
        match case {
            "missing" => tx
                .execute_batch("DROP TRIGGER staged_completion_refusals_no_update")
                .unwrap(),
            "extra" => {
                for index in 0..20 {
                    tx.execute_batch(&format!(
                        "CREATE TABLE staged_completion_extra_{index} (value TEXT)"
                    ))
                    .unwrap();
                }
            }
            "multibyte" => {
                let max = schema().iter().map(String::len).max().unwrap();
                let prefix = "CREATE TRIGGER staged_completion_refusals_no_update BEFORE UPDATE ON staged_completion_refusals BEGIN SELECT RAISE(ABORT, '";
                let suffix = "'); END";
                let sql = format!(
                    "{prefix}{}{suffix}",
                    "é".repeat(max - prefix.len() - suffix.len() - 1)
                );
                assert!(sql.chars().count() < max && sql.len() > max);
                tx.execute_batch("DROP TRIGGER staged_completion_refusals_no_update")
                    .unwrap();
                tx.execute_batch(&sql).unwrap();
            }
            _ => {}
        }
        assert_eq!(validate_schema(&tx).is_ok(), case == "canonical");
        tx.rollback().unwrap();
    }
    assert!(validate_schema(&connection).is_err());
}

struct Fixture {
    store: JobStore,
    execution: JobExecution,
    id: String,
    request: StagedObligationRequest,
    input: InvestigationInput,
    finish: StagedFinish,
    response: InvestigationResponseMeta,
    /// Dropped last: Windows cannot remove it while a store holds files in it (#1307).
    directory: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        Self::with_question("Inspect selected limits")
    }
    fn with_question(question: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
        let snapshot = context_hub::ContextSnapshot::new(vec![], vec![]).unwrap();
        let record=core_graph::coverage::CoverageRecord {repo_key:"fixture/repo".into(),registered_source_id:Some("src:fixture".into()),ingest_id:"fixture-ingest".into(),config_hash:"fixture-config".into(),inventory_json:json!({"inventory":context_hub::coverage::INVENTORY_VERSION,"repo":"fixture/repo","commit":"workdir","layer_hints":[],"producers":[],"unsupported":[],"limited":false}).to_string()};
        let stored = core_graph::coverage::StoredCoverage {
            inventory_hash: record.inventory_hash(),
            record,
        };
        let mut second = stored.clone();
        second.record.repo_key = "fixture/other".into();
        let mut body: serde_json::Value =
            serde_json::from_str(&second.record.inventory_json).unwrap();
        body["repo"] = json!("fixture/other");
        second.record.inventory_json = body.to_string();
        second.inventory_hash = second.record.inventory_hash();
        let qualification = context_hub::coverage::CoverageQualification::new(
            &[
                context_hub::coverage::ScopedRepo {
                    repo_key: "fixture/repo".into(),
                    registered_source_id: Some("src:fixture".into()),
                    config_hash: "fixture-config".into(),
                    layer_hints: None,
                },
                context_hub::coverage::ScopedRepo {
                    repo_key: "fixture/other".into(),
                    registered_source_id: Some("src:fixture".into()),
                    config_hash: "fixture-config".into(),
                    layer_hints: None,
                },
            ],
            &[stored.clone(), second.clone()],
        )
        .unwrap();
        let proof = qualification.durable_proof(snapshot.id()).unwrap();
        let archive =
            InvestigationCoverageArchive::new(proof.clone(), vec![stored.clone(), second.clone()])
                .unwrap();
        let start = StartInvestigationRequest {
            schema_version: 3,
            request_nonce: "staged-test".into(),
            specialist_id: SpecialistId::DomainAnalystV6,
            inspection_requirements: None,
            question: question.into(),
            scope: InvestigationScope::All,
            provider_mode: InvestigationProviderMode::Local,
            limit_profile: "investigation-v1".into(),
            expected_graph_revision: None,
            expected_context_id: Some(proof.context_id.clone()),
            conversation_id: None,
            parent_id: None,
        };
        let provider = InvestigationProvider {
            mode: InvestigationProviderMode::Local,
            provider_id: "local:fixture".into(),
            model: "test-model".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            deployment: None,
            protocol_version: None,
            available: true,
            unavailable_reason: None,
        };
        let detail = store
            .start_investigation(&start, Some(&provider))
            .unwrap()
            .detail;
        let id = detail.summary.investigation_id.clone();
        let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
        let plan = store
            .claim_plan(detail.summary.job_id, ClaimMode::StartQueued)
            .unwrap();
        let reservation = locks.try_reserve(plan.lock_target()).unwrap();
        let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
        let ledger = InvestigationInputLedger {
            schema_version: 2,
            coverage: Some(proof.clone()),
            coverage_reads: vec![],
            artifact_reads: vec![],
            graph_snapshot_id: snapshot.id().into(),
            scope_snapshot_id: snapshot.id().into(),
            revision: 0,
            selected_facts: vec![],
            receipt_references: vec![],
            citations: vec![],
            queries: vec![],
            supplied_history_hash: None,
            supplied_history_bytes: 0,
        };
        let detail = store.investigation(&id).unwrap();
        store
            .advance_investigation(
                &execution,
                detail.summary.revision,
                InvestigationTransition::InputPrepared {
                    coverage_archive: Some(Box::new(archive)),
                    ledger: Box::new(ledger.clone()),
                    usage: detail.usage,
                },
            )
            .unwrap();
        let input = InvestigationInput::prepare_v6(
            &start.question,
            SpecialistId::DomainAnalystV6,
            ledger.clone(),
            &[],
            &[],
            &[],
            &InvestigationUsage::default(),
            &InvestigationTurnBasis {
                input_ledger_hash: ledger.fingerprint().unwrap(),
                previous_ledger: None,
                completed_tool: None,
            },
            None,
            &[],
        )
        .unwrap();
        let action = input.action("finish-step".into()).unwrap();
        let local = OllamaProvider::new(
            "http://127.0.0.1:11434",
            "test-model",
            Duration::from_secs(1),
        )
        .unwrap();
        let prepared = EgressFirewall::new(EgressPolicy::default())
            .prepare_bounded(&local, &action, &CompletionLimits::default())
            .unwrap();
        let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
        let step = PendingInvestigationStep {
            step_id: "finish-step".into(),
            payload_hash: prepared.preview().egress.payload_hash.clone(),
            input_ledger_hash: ledger.fingerprint().unwrap(),
            input_bytes: serde_json::to_vec(&action.payload).unwrap().len(),
            generated_tokens: 2048,
            disclosure: Some(disclosure),
        };
        let detail = store.investigation(&id).unwrap();
        store
            .advance_investigation(
                &execution,
                detail.summary.revision,
                InvestigationTransition::BeginInvocation { step },
            )
            .unwrap();
        let finish = StagedFinish {
            narrative: None,
            schema_version: 1,
            outcome: CompletionOutcome::InsufficientEvidence,
            action: InvestigationAction::Finish {
                findings: vec![],
                knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
                limitations: vec!["Selected evidence is not sufficient.".into()],
            },
        };
        let canonical = serde_json::to_value(&finish.action).unwrap();
        let response = InvestigationResponseMeta {
            finish_feedback_witness: None,
            finish_telemetry: None,
            step_id: "finish-step".into(),
            response_hash: core_prov::content_hash(b"observed-complete-response"),
            action_hash: core_prov::content_hash(&serde_json::to_vec(&canonical).unwrap()),
            observed_model: Some("test-model".into()),
            reported_input_tokens: Some(10),
            reported_output_tokens: Some(5),
            reported_cache_read_tokens: None,
            active_milliseconds: 1,
        };
        let mut obligation = Obligation::Inventory {
            obligation_id: String::new(),
            target: InventoryTarget {
                context_id: proof.context_id.clone(),
                repo: "fixture/repo".into(),
                inventory_hash: stored.inventory_hash,
            },
        };
        let hash = obligation.content_id().unwrap();
        if let Obligation::Inventory { obligation_id, .. } = &mut obligation {
            *obligation_id = hash;
        }
        let request = StagedObligationRequest {
            schema_version: 4,
            request_nonce: start.request_nonce,
            specialist_id: "domain-analyst@7".into(),
            obligations: ObligationEnvelope {
                schema_version: 1,
                contract: "investigation-completion-obligations@1".into(),
                obligations: {
                    let mut other = Obligation::Inventory {
                        obligation_id: String::new(),
                        target: InventoryTarget {
                            context_id: proof.context_id.clone(),
                            repo: "fixture/other".into(),
                            inventory_hash: second.inventory_hash.clone(),
                        },
                    };
                    let hash = other.content_id().unwrap();
                    if let Obligation::Inventory { obligation_id, .. } = &mut other {
                        *obligation_id = hash;
                    }
                    vec![obligation, other]
                },
            },
            inspection_requirements: None,
            question: start.question,
            scope: start.scope,
            provider_mode: start.provider_mode,
            limit_profile: start.limit_profile,
            expected_graph_revision: None,
            expected_context_id: proof.context_id,
            conversation_id: None,
            parent_id: None,
        };
        Self {
            directory,
            store,
            execution,
            id,
            request,
            input,
            finish,
            response,
        }
    }
}
// AC-0265: real JobStore invocation + caller transaction, no provider dispatch.
// AC-0265: private journal integrity and caller-owned transaction contract.
#[test]
fn staged_refusal_journal_commit_rollback_and_fresh_store_replay_are_atomic() {
    let mut fixture = Fixture::new();
    let namespace = fixture.store.namespace.clone();
    // Initialize only the private optional namespace to observe rollback independently.
    {
        let tx = fixture.store.conn.transaction().unwrap();
        initialize(&tx).unwrap();
        tx.commit().unwrap();
    }
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert_eq!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &fixture.request,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[]
            )
            .unwrap(),
            RefusalDisposition::Continue
        );
        assert_eq!(load(&tx, &fixture.id).unwrap().len(), 1);
        tx.rollback().unwrap();
    }
    for table in [
        "staged_completion_intents",
        "staged_completion_refusals",
        "staged_completion_feedback",
        "investigation_responses",
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
    {
        let tx = fixture.store.conn.transaction().unwrap();
        append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let before = load(&fixture.store.conn, &fixture.id).unwrap();
    assert_eq!(
        before[0].record.feedback.unmet[0].reason,
        RefusalReason::NotAttempted
    );
    assert_eq!(before[0].usage.reported_input_tokens, Some(10));
    assert_eq!(before[0].usage.reported_output_tokens, Some(5));
    assert_eq!(before[0].usage.model_invocations, 1);
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    let after = load(&reopened.conn, &fixture.id).unwrap();
    assert_eq!(
        serde_json::to_value(&before).unwrap(),
        serde_json::to_value(&after).unwrap()
    );
    assert!(
        !reopened
            .investigation(&fixture.id)
            .unwrap()
            .summary
            .has_result
    );
    assert!(
        reopened
            .investigation_result(&fixture.id)
            .unwrap()
            .is_none()
    );
}
// AC-0265: private journal integrity and caller-owned transaction contract.
#[test]
fn staged_refusal_journal_rejects_binding_forged_attempt_and_tampered_assessment() {
    let mut fixture = Fixture::new();
    let namespace = fixture.store.namespace.clone();
    let mut wrong = fixture.request.clone();
    wrong.question.push('?');
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &wrong,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[]
            )
            .is_err()
        );
        tx.rollback().unwrap();
    }
    {
        let tx = fixture.store.conn.transaction().unwrap();
        append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let mut wrong = fixture.request.clone();
    wrong.request_nonce.push('x');
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &wrong,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[]
            )
            .is_err()
        );
        tx.rollback().unwrap();
    }
    let item = &fixture.request.obligations.obligations[0];
    let Obligation::Inventory { target, .. } = item else {
        unreachable!()
    };
    let ledger = fixture.input.ledger();
    let mut forged = AttemptRecord {
        schema_version: 1,
        attempt_id: String::new(),
        task_id: fixture.id.clone(),
        request_hash: fixture.request.intent_hash().unwrap(),
        obligations_hash: fixture.request.obligations.fingerprint().unwrap(),
        obligation_id: item.key().1.into(),
        step_id: "finish-step".into(),
        read: ReadIntent::Inventory {
            context_id: target.context_id.clone(),
            repo: target.repo.clone(),
            inventory_hash: target.inventory_hash.clone(),
            max_bytes: MAX_QUERY_BYTES,
        },
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_revision: 0,
        outcome: AttemptOutcome::Unavailable {
            reason:
                agents::investigation::staged_attempt::UnavailableReason::InventoryExceedsMaxBytes,
        },
    };
    forged.attempt_id = forged.content_id().unwrap();
    let mut boundary_fixture = Fixture::new();
    let tx = boundary_fixture.store.conn.transaction().unwrap();
    // AC-0188: replay uses the exact saved policy's tool-action boundary.
    // At the boundary, witnesses still need validation; above it, capacity
    // rejects the list before any claimed witness can be accepted.
    let limit = InvestigationPolicy::for_limits(
        &load_task_base(&tx, &boundary_fixture.id)
            .unwrap()
            .detail
            .limits,
    )
    .unwrap()
    .limits()
    .tool_actions as usize;
    assert!(matches!(
        completed_attempts(&tx, &boundary_fixture.id, &vec![forged.clone(); limit]),
        Err(InvestigationStoreError::Invalid)
    ));
    assert!(matches!(
        completed_attempts(&tx, &boundary_fixture.id, &vec![forged.clone(); limit + 1]),
        Err(InvestigationStoreError::Capacity)
    ));
    {
        let mut task = load_task_base(&tx, &boundary_fixture.id).unwrap();
        task.detail.limits.tool_actions += 1;
        let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
        tx.execute(
            "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
            params![boundary_fixture.id, bytes, core_prov::content_hash(&bytes)],
        )
        .unwrap();
        assert!(matches!(
            completed_attempts(&tx, &boundary_fixture.id, &[]),
            Err(InvestigationStoreError::Invalid)
        ));
        tx.rollback().unwrap();
    }
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &fixture.request,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[forged]
            )
            .is_err()
        );
        tx.rollback().unwrap();
    }
    let mut entry = load(&fixture.store.conn, &fixture.id).unwrap().remove(0);
    entry.record.assessment[0].attempted = true;
    entry.record.refusal_id = entry.record.content_id().unwrap();
    entry.entry_id = entry.content_id().unwrap();
    let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
    fixture
        .store
        .conn
        .execute_batch("DROP TRIGGER staged_completion_refusals_no_update")
        .unwrap();
    fixture
        .store
        .conn
        .execute(
            "UPDATE staged_completion_refusals SET body=?1,body_hash=?2",
            params![body, core_prov::content_hash(&body)],
        )
        .unwrap();
    fixture
        .store
        .conn
        .execute_batch(
            &schema()
                .into_iter()
                .find(|sql| sql.starts_with("CREATE TRIGGER staged_completion_refusals_no_update"))
                .unwrap(),
        )
        .unwrap();
    assert!(load(&fixture.store.conn, &fixture.id).is_err());
}

// AC-0265: private journal integrity and caller-owned transaction contract.
#[test]
fn staged_refusal_journal_rejects_response_usage_tampering() {
    for field in [
        "action_hash",
        "observed_model",
        "reported_tokens",
        "response_and_reported_tokens",
        "cache_aggregate",
        "cache_response_and_aggregate",
        "active_time",
    ] {
        let mut fixture = Fixture::new();
        let namespace = fixture.store.namespace.clone();
        {
            let tx = fixture.store.conn.transaction().unwrap();
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &fixture.request,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let mut entry = load(&fixture.store.conn, &fixture.id).unwrap().remove(0);
        match field {
            "action_hash" => entry.response.action_hash = "a".repeat(64),
            "observed_model" => entry.response.observed_model = Some("changed-model".into()),
            "reported_tokens" => entry.usage.reported_input_tokens = Some(11),
            "cache_aggregate" => entry.usage.reported_cache_read_tokens = Some(7),
            "cache_response_and_aggregate" => {
                entry.response.reported_cache_read_tokens = Some(7);
                entry.usage.reported_cache_read_tokens = Some(7);
            }
            "response_and_reported_tokens" => {
                entry.response.reported_input_tokens = Some(11);
                entry.usage.reported_input_tokens = Some(11);
            }
            _ => entry.usage.active_milliseconds += 1,
        };
        // Recompute both identity layers: replay must use the independent
        // immutable ordinary response row, not a self-consistent journal body.
        entry.entry_id = entry.content_id().unwrap();
        let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
        fixture
            .store
            .conn
            .execute_batch("DROP TRIGGER staged_completion_refusals_no_update")
            .unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE staged_completion_refusals SET body=?1,body_hash=?2",
                params![body, core_prov::content_hash(&body)],
            )
            .unwrap();
        fixture
            .store
            .conn
            .execute_batch(
                &schema()
                    .into_iter()
                    .find(|sql| {
                        sql.starts_with("CREATE TRIGGER staged_completion_refusals_no_update")
                    })
                    .unwrap(),
            )
            .unwrap();
        let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
        assert!(load(&reopened.conn, &fixture.id).is_err(), "{field}");
    }
}

// AC-0265: private journal integrity and caller-owned transaction contract.
#[test]
fn staged_refusal_journal_second_refusal_requires_terminal_and_rolls_back() {
    let mut fixture = Fixture::new();
    let namespace = fixture.store.namespace.clone();
    {
        let tx = fixture.store.conn.transaction().unwrap();
        append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    // Explicit private caller bridge only: slice 3 owns this outer phase/usage
    // transition. No production branch calls this or the staged journal yet.
    let first = load(&fixture.store.conn, &fixture.id).unwrap().remove(0);
    {
        let tx = fixture.store.conn.transaction().unwrap();
        let mut task = load_task(&tx, &fixture.id).unwrap();
        let revision = task.detail.summary.revision;
        task.phase = Phase::Ready;
        task.pending = None;
        task.approved = false;
        task.action_tool = None;
        task.detail.usage = first.usage;
        bump(&tx, &mut task).unwrap();
        save_task(&tx, &task, revision, false).unwrap();
        tx.commit().unwrap();
    }
    // An actual successful inventory read uses the production transition spine,
    // producing ToolCompleted and exactly the next ledger revision.
    let ledger_before = fixture.input.ledger().clone();
    let archive = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let archive = super::super::coverage::load(&tx, &fixture.id)
            .unwrap()
            .unwrap();
        tx.commit().unwrap();
        archive
    };
    let Obligation::Inventory { target, .. } = &fixture.request.obligations.obligations[0] else {
        unreachable!()
    };
    let read = InvestigationCoverageRead {
        context_id: target.context_id.clone(),
        repo: target.repo.clone(),
        inventory_hash: target.inventory_hash.clone(),
        max_bytes: MAX_QUERY_BYTES,
    };
    let raw = serde_json::to_string(&archive.read(&read).unwrap()).unwrap();
    let mut read_step: PendingInvestigationStep = read_body(
        &fixture.store.conn,
        "investigation_invocations",
        "step_id",
        &fixture.id,
        &"finish-step",
    )
    .unwrap()
    .unwrap();
    read_step.step_id = "actual-read-step".into();
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    fixture
        .store
        .advance_investigation(
            &fixture.execution,
            detail.summary.revision,
            InvestigationTransition::BeginInvocation { step: read_step },
        )
        .unwrap();
    let intent = ReadIntent::Inventory {
        context_id: read.context_id.clone(),
        repo: read.repo.clone(),
        inventory_hash: read.inventory_hash.clone(),
        max_bytes: read.max_bytes,
    };
    let mut read_response = fixture.response.clone();
    read_response.step_id = "actual-read-step".into();
    read_response.action_hash = read_action_hash(&intent).unwrap();
    read_response.active_milliseconds = 2;
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    fixture
        .store
        .advance_investigation(
            &fixture.execution,
            detail.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: read_response,
                tool: Some(InvestigationTool::ReadCoverageInventory),
                result: None,
            },
        )
        .unwrap();
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    fixture
        .store
        .advance_investigation(
            &fixture.execution,
            detail.summary.revision,
            InvestigationTransition::BeginTool {
                tool: InvestigationTool::ReadCoverageInventory,
            },
        )
        .unwrap();
    let mut ledger = ledger_before.clone();
    ledger.revision += 1;
    ledger
        .coverage_reads
        .push(InvestigationCoverageReadManifest {
            request: read,
            response_hash: core_prov::content_hash(raw.as_bytes()),
            response_bytes: raw.len(),
        });
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    fixture
        .store
        .advance_investigation(
            &fixture.execution,
            detail.summary.revision,
            InvestigationTransition::InputAdmitted {
                ledger: Box::new(ledger.clone()),
                usage: detail.usage,
            },
        )
        .unwrap();
    fixture.input = InvestigationInput::prepare_v6(
        &fixture.request.question,
        SpecialistId::DomainAnalystV6,
        ledger.clone(),
        &[],
        &[],
        &[],
        &InvestigationUsage::default(),
        &InvestigationTurnBasis {
            input_ledger_hash: ledger.fingerprint().unwrap(),
            previous_ledger: Some(ledger_before),
            completed_tool: Some(InvestigationTool::ReadCoverageInventory),
        },
        None,
        &[raw],
    )
    .unwrap();
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let action = fixture.input.action("second-finish-step".into()).unwrap();
    let prepared = EgressFirewall::new(EgressPolicy::default())
        .prepare_bounded(&provider, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = fixture.input.seal_disclosure(&prepared).unwrap().unwrap();
    let mut completed = AttemptRecord {
        schema_version: 1,
        attempt_id: String::new(),
        task_id: fixture.id.clone(),
        request_hash: fixture.request.intent_hash().unwrap(),
        obligations_hash: fixture.request.obligations.fingerprint().unwrap(),
        obligation_id: fixture.request.obligations.obligations[0].key().1.into(),
        step_id: "actual-read-step".into(),
        read: intent,
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_revision: ledger.revision,
        outcome: AttemptOutcome::InventoryAcquired { read_index: 0 },
    };
    completed.attempt_id = completed.content_id().unwrap();
    let mut step: PendingInvestigationStep = read_body(
        &fixture.store.conn,
        "investigation_invocations",
        "step_id",
        &fixture.id,
        &"finish-step",
    )
    .unwrap()
    .unwrap();
    step.step_id = "second-finish-step".into();
    step.input_ledger_hash = ledger.fingerprint().unwrap();
    step.payload_hash = prepared.preview().egress.payload_hash.clone();
    step.disclosure = Some(disclosure);
    step.input_bytes = serde_json::to_vec(&action.payload).unwrap().len();
    let detail = fixture.store.investigation(&fixture.id).unwrap();
    fixture
        .store
        .advance_investigation(
            &fixture.execution,
            detail.summary.revision,
            InvestigationTransition::BeginInvocation { step },
        )
        .unwrap();
    fixture.response.step_id = "second-finish-step".into();
    fixture.response.active_milliseconds = 3;
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert_eq!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &fixture.request,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                std::slice::from_ref(&completed)
            )
            .unwrap(),
            RefusalDisposition::TerminalRequired
        );
        let replayed = load(&tx, &fixture.id).unwrap();
        assert_eq!(replayed.len(), 2);
        // First refusal + actual read + second refusal: each admitted response
        // contributes exactly once, including the intervening tool response.
        assert_eq!(replayed[1].usage.model_invocations, 3);
        assert_eq!(replayed[1].usage.reported_input_tokens, Some(30));
        assert_eq!(replayed[1].usage.reported_output_tokens, Some(15));
        let responses: i64 = tx
            .query_row(
                "SELECT count(*) FROM investigation_responses WHERE task_id=?1",
                [&fixture.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(responses, 3);
        assert!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &fixture.request,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                std::slice::from_ref(&completed)
            )
            .is_err()
        );
        tx.rollback().unwrap();
    }
    let committed = load(&fixture.store.conn, &fixture.id).unwrap();
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].record.refusal_count, 1);
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM staged_completion_feedback WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let responses: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM investigation_responses WHERE task_id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        responses, 2,
        "second refusal response rolls back with its journal"
    );
}

// AC-0265: original request identity cannot be replaced by its redacted display.
#[test]
fn staged_refusal_journal_exact_intent_rejects_redaction_collisions() {
    let original = "Inspect password=firstsecret123 bounded evidence";
    let alias = "Inspect password=othersecret456 bounded evidence";
    assert_eq!(
        redacted_history_text(original, MAX_QUESTION_BYTES).unwrap(),
        redacted_history_text(alias, MAX_QUESTION_BYTES).unwrap()
    );
    let mut fixture = Fixture::with_question(original);
    let namespace = fixture.store.namespace.clone();
    let mut changed = fixture.request.clone();
    changed.question = alias.into();
    assert_ne!(
        changed.intent_hash().unwrap(),
        fixture.request.intent_hash().unwrap()
    );
    {
        let tx = fixture.store.conn.transaction().unwrap();
        assert!(
            append(
                &tx,
                &namespace,
                &fixture.execution,
                &changed,
                &fixture.input,
                &fixture.finish,
                fixture.response.clone(),
                &[]
            )
            .is_err()
        );
        tx.rollback().unwrap();
    }
    {
        let tx = fixture.store.conn.transaction().unwrap();
        append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        )
        .unwrap();
        tx.rollback().unwrap();
    }
}

// AC-0265: ordinary shared capacity checks include every committed journal row,
// not just the private append path, with identical per-task/global charging.
#[test]
fn staged_refusal_journal_counts_toward_ordinary_capacity_after_commit() {
    let mut fixture = Fixture::new();
    let task_before = stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap();
    let global_before = stored_bytes(&fixture.store.conn, None).unwrap();
    let task_remaining = remaining_task_capacity(&fixture.store.conn, &fixture.id).unwrap();
    let global_remaining = remaining_store_capacity(&fixture.store.conn).unwrap();
    let namespace = fixture.store.namespace.clone();
    let tx = fixture.store.conn.transaction().unwrap();
    append(
        &tx,
        &namespace,
        &fixture.execution,
        &fixture.request,
        &fixture.input,
        &fixture.finish,
        fixture.response.clone(),
        &[],
    )
    .unwrap();
    tx.commit().unwrap();
    let mut charged = 0usize;
    for table in [
        "staged_completion_intents",
        "staged_completion_refusals",
        "staged_completion_feedback",
    ] {
        let bytes: i64 = fixture.store.conn.query_row(
            &format!("SELECT sum(length(CAST(body AS BLOB))+length(CAST(body_hash AS BLOB))+256) FROM {table} WHERE task_id=?1"),
            [&fixture.id], |row| row.get(0)).unwrap();
        charged += usize::try_from(bytes).unwrap();
    }
    assert!(charged > 0);
    let response_charge = encode(&fixture.response, 4096).unwrap().len() + 64 + 256;
    charged += response_charge;
    assert_eq!(
        stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap(),
        task_before + charged
    );
    assert_eq!(
        stored_bytes(&fixture.store.conn, None).unwrap(),
        global_before + charged
    );
    assert_eq!(
        remaining_task_capacity(&fixture.store.conn, &fixture.id).unwrap(),
        task_remaining - charged
    );
    assert_eq!(
        remaining_store_capacity(&fixture.store.conn).unwrap(),
        global_remaining - charged
    );
    let task = {
        let tx = fixture.store.conn.unchecked_transaction().unwrap();
        let task = load_task(&tx, &fixture.id).unwrap();
        tx.commit().unwrap();
        task
    };
    // This ordinary-body padding fits exactly under the old journal-blind
    // accounting. SQLite supplies its bytes, without a large owned Rust buffer.
    let padding = TASK_CAPACITY - TERMINAL_RESERVE - task_before - response_charge - 64 - 256;
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO investigation_events VALUES (?1,999,zeroblob(?2),?3)",
            params![fixture.id, i64::try_from(padding).unwrap(), "0".repeat(64)],
        )
        .unwrap();
    assert_eq!(
        task_before + response_charge + padding + 64 + 256,
        TASK_CAPACITY - TERMINAL_RESERVE
    );
    assert_eq!(
        check_task_capacity(&fixture.store.conn, &fixture.id, false),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(
        remaining_task_capacity(&fixture.store.conn, &fixture.id),
        Err(InvestigationStoreError::Capacity)
    );
    // A real ordinary coordinator write is refused and rolled back as well.
    let before: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    let tx = fixture.store.conn.transaction().unwrap();
    assert_eq!(
        save_task(&tx, &task, task.detail.summary.revision, false),
        Err(InvestigationStoreError::Capacity)
    );
    tx.rollback().unwrap();
    assert_eq!(
        fixture.store.cancel_investigation(&fixture.id),
        Err(InvestigationStoreError::Capacity)
    );
    let after: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
}

// AC-0265: capacity reads neither create optional tables nor charge a legacy
// store differently when the journal is absent or initialized but empty.
#[test]
fn staged_refusal_journal_capacity_preserves_legacy_without_optional_tables() {
    let mut fixture = Fixture::new();
    let task_before = stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap();
    let global_before = stored_bytes(&fixture.store.conn, None).unwrap();
    assert_eq!(
        journal_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap(),
        0
    );
    let count: i64 = fixture
        .store
        .conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name GLOB 'staged_completion_*'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        remaining_task_capacity(&fixture.store.conn, &fixture.id).unwrap(),
        TASK_CAPACITY - TERMINAL_RESERVE - task_before
    );
    assert_eq!(
        remaining_store_capacity(&fixture.store.conn).unwrap(),
        STORE_CAPACITY - global_before
    );
    let tx = fixture.store.conn.transaction().unwrap();
    initialize(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap(),
        task_before
    );
    assert_eq!(
        stored_bytes(&fixture.store.conn, None).unwrap(),
        global_before
    );
    fixture.store.cancel_investigation(&fixture.id).unwrap();
}

// AC-0265: token replay follows invocation order, requires every ordinary
// response and preserves unknown totals across intervening admitted reads.
#[test]
fn staged_refusal_response_replay_requires_complete_history_and_preserves_unknown() {
    for unknown_at in [None, Some(0), Some(1)] {
        let mut fixture = Fixture::new();
        let tx = fixture.store.conn.transaction().unwrap();
        let mut step: PendingInvestigationStep = read_body(
            &tx,
            "investigation_invocations",
            "step_id",
            &fixture.id,
            &"finish-step",
        )
        .unwrap()
        .unwrap();
        for ordinal in 0..3 {
            let mut response = fixture.response.clone();
            if ordinal > 0 {
                step.step_id = format!("history-step-{ordinal}");
                insert_body(
                    &tx,
                    "investigation_invocations",
                    &fixture.id,
                    &step.step_id,
                    &step,
                    MAX_INVOCATION_BYTES,
                )
                .unwrap();
            }
            response.step_id = step.step_id.clone();
            response.reported_input_tokens = if unknown_at == Some(ordinal) {
                None
            } else {
                Some(10 * (ordinal as u64 + 1))
            };
            response.reported_output_tokens = Some(ordinal as u64 + 1);
            response.reported_cache_read_tokens = (unknown_at != Some(ordinal)).then_some(7);
            insert_body(
                &tx,
                "investigation_responses",
                &fixture.id,
                &response.step_id,
                &response,
                4096,
            )
            .unwrap();
        }
        let replay = response_usage(&tx, &fixture.id, "history-step-2", 8).unwrap();
        assert_eq!(replay.invocations, 3);
        assert_eq!(
            replay.input_tokens,
            if unknown_at.is_some() { None } else { Some(60) }
        );
        assert_eq!(replay.output_tokens, Some(6));
        assert_eq!(
            replay.cache_read_tokens,
            if unknown_at.is_some() { None } else { Some(21) }
        );
        assert!(
            insert_body(
                &tx,
                "investigation_responses",
                &fixture.id,
                &fixture.response.step_id,
                &fixture.response,
                4096
            )
            .is_err()
        );
        tx.execute_batch("DROP TRIGGER investigation_responses_no_delete")
            .unwrap();
        tx.execute(
            "DELETE FROM investigation_responses WHERE task_id=?1 AND step_id='finish-step'",
            [&fixture.id],
        )
        .unwrap();
        assert!(response_usage(&tx, &fixture.id, "history-step-2", 8).is_err());
        tx.rollback().unwrap();
    }
}

#[test]
fn staged_binding_row_caps_preserve_semantic_field_validation() {
    // AC-0266: the V2 outer record cap grants no authority to substituted or
    // unbounded binding fields. Existing v1 persisted bindings remain 32 KiB.
    let mut fixture = Fixture::new();
    let namespace = fixture.store.namespace.clone();
    let tx = fixture.store.conn.transaction().unwrap();
    let task = load_task_base(&tx, &fixture.id).unwrap();
    let original = bind(&tx, &task, &fixture.request).unwrap();
    assert_eq!(binding_bytes(&INVESTIGATION_V1_POLICY), 32 * 1024);
    assert_eq!(binding_bytes(&INVESTIGATION_V2_POLICY), MAX_RECORD_BYTES);
    assert_eq!(journal_entry_bytes(&INVESTIGATION_V1_POLICY), 64 * 1024);
    assert_eq!(
        journal_entry_bytes(&INVESTIGATION_V2_POLICY),
        MAX_RECORD_BYTES
    );
    for field in ["request_hash", "context", "graph", "question", "envelope"] {
        let mut binding = original.clone();
        match field {
            "request_hash" => binding.request_hash = "0".repeat(65),
            "context" => binding.expected_context = "x".repeat(64 * 1024),
            "graph" => binding.expected_graph = Some("0".repeat(64)),
            "question" => binding.redacted_question_hash = "0".repeat(64),
            _ => binding.envelope.contract = "unknown".into(),
        }
        encode(&binding, binding_bytes(&INVESTIGATION_V2_POLICY)).unwrap();
        assert_eq!(
            binding.validate(&task),
            Err(InvestigationStoreError::Invalid),
            "{field}"
        );
    }
    // The original binding still supports the ordinary production transition.
    append(
        &tx,
        &namespace,
        &fixture.execution,
        &fixture.request,
        &fixture.input,
        &fixture.finish,
        fixture.response.clone(),
        &[],
    )
    .unwrap();
    tx.rollback().unwrap();
}

// AC-0418: absent extensions preserve canonical bytes and content IDs; null is not absence.
#[test]
fn cache_usage_legacy_bytes_and_present_widths_are_canonical() {
    let mut fixture = Fixture::new();
    let legacy = encode(&fixture.response, 4096).unwrap();
    let expected = format!(
        "{{\"step_id\":\"finish-step\",\"response_hash\":\"{}\",\"action_hash\":\"{}\",\"observed_model\":\"test-model\",\"reported_input_tokens\":10,\"reported_output_tokens\":5,\"active_milliseconds\":1}}",
        fixture.response.response_hash, fixture.response.action_hash
    );
    assert_eq!(legacy, expected.as_bytes());
    assert_eq!(
        decode::<InvestigationResponseMeta>(&legacy, &core_prov::content_hash(expected.as_bytes()))
            .unwrap(),
        fixture.response
    );
    let usage = InvestigationUsage::default();
    let body = encode(&usage, 4096).unwrap();
    assert!(
        !String::from_utf8(body.clone())
            .unwrap()
            .contains("cache_read")
    );
    assert_eq!(
        decode::<InvestigationUsage>(&body, &core_prov::content_hash(&body)).unwrap(),
        usage
    );
    for value in [serde_json::Value::Null, json!(0), json!(u64::MAX)] {
        for response in [false, true] {
            // Keep struct field order, inserting immediately before the final active counter.
            let original = if response { &legacy } else { &body };
            let wire = String::from_utf8(original.clone()).unwrap().replace(
                "\"active_milliseconds\":",
                &format!("\"reported_cache_read_tokens\":{value},\"active_milliseconds\":"),
            );
            let hash = core_prov::content_hash(wire.as_bytes());
            let accepted = if response {
                decode::<InvestigationResponseMeta>(wire.as_bytes(), &hash).is_ok()
            } else {
                decode::<InvestigationUsage>(wire.as_bytes(), &hash).is_ok()
            };
            assert_eq!(accepted, !value.is_null());
        }
    }
    let namespace = fixture.store.namespace.clone();
    let tx = fixture.store.conn.transaction().unwrap();
    append(
        &tx,
        &namespace,
        &fixture.execution,
        &fixture.request,
        &fixture.input,
        &fixture.finish,
        fixture.response.clone(),
        &[],
    )
    .unwrap();
    let entry = load(&tx, &fixture.id).unwrap().remove(0);
    let body = encode(&entry, MAX_RECORD_BYTES).unwrap();
    assert!(
        !String::from_utf8(body.clone())
            .unwrap()
            .contains("cache_read")
    );
    let decoded: JournalEntry = decode(&body, &core_prov::content_hash(&body)).unwrap();
    assert_eq!(decoded.content_id().unwrap(), entry.entry_id);
    tx.commit().unwrap();
    let reopened = JobStore::open(fixture.directory.path().join("state.db")).unwrap();
    assert_eq!(
        load(&reopened.conn, &fixture.id).unwrap()[0].entry_id,
        entry.entry_id
    );
    assert_eq!(
        transitions::add_reported(Some(u64::MAX), Some(1), false),
        Err(InvestigationStoreError::Capacity)
    );
    assert_eq!(
        transitions::add_reported(None, Some(7), false).unwrap(),
        None
    );
}

// AC-0418: maximum reported metadata consumes existing admission capacity atomically.
#[test]
fn cache_usage_refusal_capacity_exact_and_one_short_rollback() {
    for deficit in [0, 1] {
        let mut fixture = Fixture::new();
        fixture.response.reported_cache_read_tokens = Some(u64::MAX);
        let namespace = fixture.store.namespace.clone();
        // Establish the existing binding before measuring response admission.
        let tx = fixture.store.conn.transaction().unwrap();
        let task = load_task_base(&tx, &fixture.id).unwrap();
        bind(&tx, &task, &fixture.request).unwrap();
        tx.commit().unwrap();
        let tx = fixture.store.conn.transaction().unwrap();
        append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        )
        .unwrap();
        let entry = load(&tx, &fixture.id).unwrap().remove(0);
        let charge = encode(&entry, MAX_RECORD_BYTES).unwrap().len()
            + entry
                .record
                .feedback
                .bytes_with_policy(&agents::investigation::INVESTIGATION_V1_POLICY)
                .unwrap()
                .len()
            + 1024
            + encode(&entry.response, 4096).unwrap().len()
            + 64
            + 256;
        tx.rollback().unwrap();
        let before = stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap();
        let padding = TASK_CAPACITY - TERMINAL_RESERVE - before - charge - 64 - 256 + deficit;
        // Disposable ordinary-body padding exercises the real admission check; it is not read as a journal event.
        fixture.store.conn.execute("INSERT INTO investigation_responses VALUES (?1,'capacity-padding',zeroblob(?2),?3)", params![fixture.id, i64::try_from(padding).unwrap(), "0".repeat(64)]).unwrap();
        let tx = fixture.store.conn.transaction().unwrap();
        let before_task = load_task_base(&tx, &fixture.id).unwrap();
        let result = append(
            &tx,
            &namespace,
            &fixture.execution,
            &fixture.request,
            &fixture.input,
            &fixture.finish,
            fixture.response.clone(),
            &[],
        );
        if deficit == 0 {
            assert_eq!(result.unwrap(), RefusalDisposition::Continue);
            assert_eq!(
                load(&tx, &fixture.id).unwrap()[0]
                    .usage
                    .reported_cache_read_tokens,
                Some(u64::MAX)
            );
        } else {
            assert_eq!(result.unwrap_err(), InvestigationStoreError::Capacity);
        }
        tx.rollback().unwrap();
        let tx = fixture.store.conn.transaction().unwrap();
        assert_eq!(
            load_task_base(&tx, &fixture.id).unwrap().detail,
            before_task.detail
        );
        tx.rollback().unwrap();
        assert_eq!(
            stored_bytes(&fixture.store.conn, Some(&fixture.id)).unwrap(),
            before + padding + 64 + 256
        );
        assert_eq!(fixture.store.conn.query_row("SELECT count(*) FROM investigation_responses WHERE task_id=?1 AND step_id='finish-step'", [&fixture.id], |row| row.get::<_,i64>(0)).unwrap(), 0);
    }
}
