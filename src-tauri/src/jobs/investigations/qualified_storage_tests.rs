use super::*;
use crate::job_execution::JobExecutionLocks;
use crate::jobs::ClaimMode;
use context_hub::{
    ContextSnapshot,
    coverage::{CoverageQualification, INVENTORY_VERSION, ScopedRepo},
};
use core_graph::coverage::{CoverageRecord, StoredCoverage};
use core_prov::content_hash;
use llm::bounded::CompletionLimits;
use llm::{EgressFirewall, EgressPolicy, OllamaProvider};
use serde_json::json;
use std::time::Duration;

struct Fixture {
    store: JobStore,
    id: String,
    ledger: InvestigationInputLedger,
    result: InvestigationResult,
    step: PendingInvestigationStep,
    /// Dropped last: Windows cannot remove it while a store holds files in it (#1307).
    directory: tempfile::TempDir,
}
fn archive() -> InvestigationCoverageArchive {
    let record=CoverageRecord {repo_key:"acme/api".into(),registered_source_id:Some("src:fixture".into()),
        ingest_id:"job:original".into(),config_hash:"plugins:fixture".into(),inventory_json:json!({
        "inventory":INVENTORY_VERSION,"repo":"acme/api","commit":"a".repeat(40),"layer_hints":[],"producers":[],"unsupported":[],"limited":false}).to_string()};
    let stored = StoredCoverage {
        inventory_hash: record.inventory_hash(),
        record,
    };
    let qualification = CoverageQualification::new(
        &[ScopedRepo {
            repo_key: "acme/api".into(),
            registered_source_id: Some("src:fixture".into()),
            config_hash: "plugins:fixture".into(),
            layer_hints: None,
        }],
        std::slice::from_ref(&stored),
    )
    .unwrap();
    let snapshot = ContextSnapshot::new(vec![], vec![]).unwrap();
    InvestigationCoverageArchive::new(
        qualification.durable_proof(snapshot.id()).unwrap(),
        vec![stored],
    )
    .unwrap()
}
fn target(ledger: &InvestigationInputLedger) -> InvestigationInspectionTarget {
    let mut target:InvestigationInspectionTarget=serde_json::from_value(json!({
        "schema_version":1,"target_id":"","graph_snapshot_id":ledger.graph_snapshot_id,
        "fact":{"kind":"node","id":"node:checkout"},"fact_digest":format!("node-v1:{}","a".repeat(64)),
        "registered_source_id":format!("src_{}","d".repeat(32)),"receipt_id":format!("ts-primary-v2:{}","c".repeat(64)),
        "receipt_inventory_index":3,"role":"provenance","index":0,
        "source":{"repo":"acme/api","path":"guard.ts","byte_start":0,"byte_end":12,"commit_sha":"a".repeat(40)},
        "captured":{"file":{"source_id":format!("src_{}","d".repeat(32)),"capture_id":format!("capture-v1:{}","e".repeat(64)),
        "path":"guard.ts","digest":content_hash(b"private text"),"byte_len":12},"byte_start":0,"byte_end":12}
    })).unwrap();
    target.target_id = target.content_id().unwrap();
    target.validate().unwrap();
    target
}
fn fixture(targeted: bool) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let archive = archive();
    let proof = archive.proof();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(proof.clone()),
        coverage_reads: vec![],
        artifact_reads: vec![],
        graph_snapshot_id: proof.snapshot_id.clone(),
        scope_snapshot_id: proof.snapshot_id.clone(),
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    let targets = targeted.then(|| vec![target(&ledger)]);
    let mut request = json!({"schema_version":3,"request_nonce":"qualified-storage","specialist_id":"domain-analyst@6",
        "question":"Inspect saved coverage.","scope":{"type":"all"},"provider_mode":"local","limit_profile":"investigation-v1",
        "expected_graph_revision":ledger.graph_snapshot_id,"expected_context_id":proof.context_id});
    if let Some(targets) = &targets {
        request["inspection_requirements"] = serde_json::to_value(targets).unwrap();
    }
    let request: StartInvestigationRequest = serde_json::from_value(request).unwrap();
    let provider:InvestigationProvider=serde_json::from_value(json!({"mode":"local","provider_id":"fixture",
        "model":"fixture-model","endpoint":"http://127.0.0.1:11434","deployment":null,"available":true,"unavailable_reason":null})).unwrap();
    let started = store
        .start_investigation(&request, Some(&provider))
        .unwrap()
        .detail;
    assert!(started.coverage_context.is_none());
    let id = started.summary.investigation_id;
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let plan = store
        .claim_plan(started.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    let current = store.investigation(&id).unwrap();
    store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::InputPrepared {
                ledger: Box::new(ledger.clone()),
                usage: InvestigationUsage::default(),
                coverage_archive: Some(Box::new(archive)),
            },
        )
        .unwrap();
    let basis = store.investigation_turn_basis(&id).unwrap();
    let input = InvestigationInput::prepare_v6(
        "Inspect saved coverage.",
        SpecialistId::DomainAnalystV6,
        ledger.clone(),
        &[],
        &[],
        &[],
        &InvestigationUsage::default(),
        &basis,
        targets.as_deref(),
        &[],
    )
    .unwrap();
    let local = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "fixture-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let action = input.action(format!("{id}:step:1")).unwrap();
    let prepared = firewall
        .prepare_bounded(&local, &action, &CompletionLimits::default())
        .unwrap();
    let disclosure = input.seal_disclosure(&prepared).unwrap().unwrap();
    let step = PendingInvestigationStep {
        step_id: "step:1".into(),
        payload_hash: prepared.preview().egress.payload_hash.clone(),
        input_ledger_hash: ledger.fingerprint().unwrap(),
        input_bytes: serde_json::to_vec(&prepared.preview().egress.payload)
            .unwrap()
            .len(),
        generated_tokens: 2048,
        disclosure: Some(disclosure.clone()),
    };
    let current = store.investigation(&id).unwrap();
    store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::BeginInvocation { step: step.clone() },
        )
        .unwrap();
    let finish = InvestigationAction::Finish {
        findings: vec![],
        knowledge_completeness: KnowledgeCompleteness::InsufficientEvidence,
        limitations: vec!["No relevant source was acquired; coverage remains bounded.".into()],
    };
    let result = input
        .admit_result_for_step(
            &id,
            &step.step_id,
            Some(&disclosure),
            &finish,
            None,
            "2026-10-19T00:00:00Z".into(),
        )
        .unwrap();
    let current = store.investigation(&id).unwrap();
    store
        .advance_investigation(
            &execution,
            current.summary.revision,
            InvestigationTransition::ActionAdmitted {
                response: InvestigationResponseMeta {
                    finish_feedback_witness: None,
                    finish_telemetry: None,
                    step_id: step.step_id.clone(),
                    response_hash: content_hash(b"fixture response"),
                    action_hash: content_hash(&serde_json::to_vec(&finish).unwrap()),
                    observed_model: None,
                    reported_input_tokens: None,
                    reported_output_tokens: None,
                    reported_cache_read_tokens: None,
                    active_milliseconds: 1,
                },
                tool: None,
                result: Some(Box::new(result.clone())),
            },
        )
        .unwrap();
    Fixture {
        directory,
        store,
        id,
        ledger,
        result,
        step,
    }
}

// Restore the exact trigger SQL after simulating disk corruption. This preserves
// schema validation and valid row hashes, exercising semantic reopen checks.
fn replace_body(connection: &Connection, table: &str, id: &str, body: &[u8]) {
    let trigger = format!("{table}_no_update");
    let sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE name=?1",
            [&trigger],
            |r| r.get(0),
        )
        .unwrap();
    connection
        .execute_batch(&format!("DROP TRIGGER {trigger}"))
        .unwrap();
    connection
        .execute(
            &format!("UPDATE {table} SET body=?1,body_hash=?2 WHERE task_id=?3"),
            params![body, content_hash(body), id],
        )
        .unwrap();
    connection.execute_batch(&sql).unwrap();
}
fn rehash(result: &mut InvestigationResult) {
    result.result_id.clear();
    result.result_id = format!("investigation-result-v{}:{}", result.schema_version, {
        let mut bytes = format!(
            "cartograph:investigation-result:v{}\0",
            result.schema_version
        )
        .into_bytes();
        bytes.extend(serde_json::to_vec(result).unwrap());
        content_hash(&bytes)
    });
}

// AC-0239: saved schema3 summary, ledger2 and finishing disclosure3/result4
// survive restart in both coverage-only and explicitly targeted modes.
#[test]
fn qualified_storage_reopens_exact_result_with_optional_independent_assessment() {
    for targeted in [false, true] {
        let fixture = fixture(targeted);
        assert_eq!(fixture.result.schema_version, 4);
        let before = fixture.store.investigation(&fixture.id).unwrap();
        let saved_context = before.coverage_context.as_ref().unwrap();
        let proof = fixture.ledger.coverage.as_ref().unwrap();
        assert_eq!(saved_context.context_id, proof.context_id);
        assert_eq!(saved_context.coverage, proof.qualification);
        let public = serde_json::to_value(saved_context).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 2);
        assert!(public["coverage"].get("basis").is_none());
        assert!(!public.to_string().contains("registered_source_id"));
        assert!(!public.to_string().contains("ingest_id"));
        assert!(!public.to_string().contains("config_hash"));
        let mut injected = public;
        injected["basis"] = json!([]);
        assert!(serde_json::from_value::<InvestigationCoverageContext>(injected).is_err());
        assert_eq!(before.summary.schema_version, 3);
        assert_eq!(before.summary.status, InvestigationStatus::Completed);
        assert_eq!(before.inspection_assessment.is_some(), targeted);
        if targeted {
            assert_eq!(
                before.summary.inspection_status,
                Some(InvestigationInspectionStatus::Unmet)
            );
        } else {
            assert!(before.summary.inspection_status.is_none());
        }
        let path = fixture.directory.path().join("state.db");
        drop(fixture.store);
        let reopened = JobStore::open(path).unwrap();
        assert_eq!(reopened.investigation(&fixture.id).unwrap(), before);
        assert_eq!(
            reopened.investigation_result(&fixture.id).unwrap(),
            Some(fixture.result.clone())
        );
        let history = reopened.investigation_history(None, None).unwrap();
        assert_eq!(history.items[0], before.summary);
        assert_eq!(
            reopened.investigation_ledger(&fixture.id).unwrap(),
            Some(fixture.ledger)
        );
    }
}

// AC-0239: valid SQL shape/row hashes and a newly computed result identity do
// not permit qualification substitution, downgrade or fabricated assessment.
#[test]
fn qualified_storage_refuses_rehashed_result_tampering_on_detail_history_and_result_reads() {
    for mutation in ["coverage", "missing", "downgrade", "inspection"] {
        let fixture = fixture(mutation == "inspection");
        let mut changed = fixture.result.clone();
        match mutation {
            "coverage" => {
                changed.coverage.as_mut().unwrap().representation_hash =
                    content_hash(b"other public block")
            }
            "missing" => changed.coverage = None,
            "downgrade" => {
                changed.schema_version = 2;
                changed.coverage = None;
            }
            _ => changed.inspection_assessment = None,
        }
        rehash(&mut changed);
        replace_body(
            &fixture.store.conn,
            "investigation_results",
            &fixture.id,
            &serde_json::to_vec(&changed).unwrap(),
        );
        let path = fixture.directory.path().join("state.db");
        drop(fixture.store);
        let reopened = JobStore::open(path).unwrap();
        assert!(reopened.investigation(&fixture.id).is_err());
        assert!(reopened.investigation_history(None, None).is_err());
        assert!(reopened.investigation_result(&fixture.id).is_err());
    }
}

// AC-0239: a coherent result is insufficient without its exact retained proof
// and finishing disclosure; reopen never recovers these from current coverage.
#[test]
fn qualified_storage_refuses_saved_archive_or_finishing_disclosure_substitution() {
    for mutation in ["archive", "disclosure", "missing_disclosure"] {
        let fixture = fixture(false);
        if mutation == "archive" {
            let body = serde_json::to_string(&archive()).unwrap().replacen(
                "\"ingest_id\":\"job:original\"",
                "\"ingest_id\":\"job:edited\"",
                1,
            );
            replace_body(
                &fixture.store.conn,
                "investigation_coverage_archives",
                &fixture.id,
                body.as_bytes(),
            );
        } else {
            let mut step = fixture.step;
            if mutation == "disclosure" {
                step.disclosure
                    .as_mut()
                    .unwrap()
                    .coverage
                    .as_mut()
                    .unwrap()
                    .context_id = "context-v2:wrong".into();
            } else {
                step.disclosure = None;
            }
            replace_body(
                &fixture.store.conn,
                "investigation_invocations",
                &fixture.id,
                &serde_json::to_vec(&step).unwrap(),
            );
        }
        let path = fixture.directory.path().join("state.db");
        drop(fixture.store);
        let reopened = JobStore::open(path).unwrap();
        assert!(reopened.investigation(&fixture.id).is_err());
        assert!(reopened.investigation_result(&fixture.id).is_err());
    }
}

// AC-0239: changing displayed specialist metadata cannot disable result/proof
// validation and turn a qualified historical record into an unchecked legacy one.
#[test]
fn qualified_storage_rejects_task_specialist_downgrade_without_losing_legacy_validation() {
    let fixture = fixture(false);
    let raw: Vec<u8> = fixture
        .store
        .conn
        .query_row(
            "SELECT record FROM investigation_tasks WHERE id=?1",
            [&fixture.id],
            |row| row.get(0),
        )
        .unwrap();
    let mut task: StoredTask = serde_json::from_slice(&raw).unwrap();
    task.detail.coverage_context = None;
    task.detail.summary.schema_version = 1;
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV3;
    task.detail.specialist = SpecialistId::DomainAnalystV3.definition().unwrap();
    let body = serde_json::to_vec(&task).unwrap();
    fixture
        .store
        .conn
        .execute(
            "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
            params![body, content_hash(&body), fixture.id],
        )
        .unwrap();
    let path = fixture.directory.path().join("state.db");
    drop(fixture.store);
    let reopened = JobStore::open(path).unwrap();
    assert!(reopened.investigation(&fixture.id).is_err());
    assert!(reopened.investigation_history(None, None).is_err());
    assert!(reopened.investigation_result(&fixture.id).is_err());
}

// AC-0239: the public historical context must be complete and exactly the
// retained proof, even when the task body and row hash were coherently rewritten.
#[test]
fn qualified_storage_rejects_dropped_or_changed_saved_public_context() {
    for mutation in ["missing", "context_id", "qualification"] {
        let fixture = fixture(false);
        let raw: Vec<u8> = fixture
            .store
            .conn
            .query_row(
                "SELECT record FROM investigation_tasks WHERE id=?1",
                [&fixture.id],
                |row| row.get(0),
            )
            .unwrap();
        let mut task: StoredTask = serde_json::from_slice(&raw).unwrap();
        match mutation {
            "missing" => task.detail.coverage_context = None,
            "context_id" => {
                task.detail.coverage_context.as_mut().unwrap().context_id =
                    format!("context-v2:{}", "f".repeat(64))
            }
            _ => task
                .detail
                .coverage_context
                .as_mut()
                .unwrap()
                .coverage
                .repos[0]
                .compared
                .clear(),
        }
        let body = serde_json::to_vec(&task).unwrap();
        fixture
            .store
            .conn
            .execute(
                "UPDATE investigation_tasks SET record=?1,record_hash=?2 WHERE id=?3",
                params![body, content_hash(&body), fixture.id],
            )
            .unwrap();
        let path = fixture.directory.path().join("state.db");
        drop(fixture.store);
        let reopened = JobStore::open(path).unwrap();
        assert!(reopened.investigation(&fixture.id).is_err());
        assert!(reopened.investigation_history(None, None).is_err());
        assert!(reopened.investigation_result(&fixture.id).is_err());
    }
}

// AC-0239: the preparation ceiling uses current task/global charges, reserves
// terminal space and the exact archive row charge, and never copies payloads.
#[test]
fn qualified_storage_capacity_preflight_is_read_only_and_counts_existing_bytes() {
    let fixture = fixture(false);
    let changes: i64 = fixture
        .store
        .conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    let before = fixture
        .store
        .investigation_coverage_archive_capacity(&fixture.id)
        .unwrap();
    let after_changes: i64 = fixture
        .store
        .conn
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(changes, after_changes);
    assert!(before < 2 * 1024 * 1024 - 256 * 1024);
    assert_eq!(
        fixture
            .store
            .investigation_coverage_archive_capacity("investigation:missing"),
        Err(InvestigationStoreError::Missing)
    );
    // SQLite creates the body; no Rust Vec/String materializes it. Capacity is
    // still charged even for unreadable payloads, before subsequent body reads.
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO investigation_events VALUES (?1,999,zeroblob(16384),?2)",
            params![fixture.id, "0".repeat(64)],
        )
        .unwrap();
    let after = fixture
        .store
        .investigation_coverage_archive_capacity(&fixture.id)
        .unwrap();
    assert_eq!(before - after, 16384 + 64 + 256);
    fixture
        .store
        .conn
        .execute(
            "INSERT INTO investigation_events VALUES (?1,1000,zeroblob(2097152),?2)",
            params![fixture.id, "0".repeat(64)],
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .investigation_coverage_archive_capacity(&fixture.id),
        Err(InvestigationStoreError::Capacity)
    );
}
