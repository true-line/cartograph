use super::*;
use crate::jobs::investigations::{InvestigationStopReason, InvestigationTransition};
use std::sync::atomic::Ordering;

fn binding() -> PreparationBinding {
    PreparationBinding {
        run_id: "v7-preparation-test".into(),
        protocol_hash: "a".repeat(64),
        source_commit: "b".repeat(40),
        source_tree: "c".repeat(40),
    }
}
fn descriptor_only() -> (super::super::super::super::Scripted, InvestigationProvider) {
    let provider =
        super::super::super::super::Scripted::new(super::super::super::super::Mode::Finish);
    let descriptor =
        crate::investigations::runtime::descriptor(&provider, InvestigationProviderMode::Local)
            .unwrap();
    (provider, descriptor)
}
fn task_count(app_data: &Path) -> i64 {
    rusqlite::Connection::open(app_data.join("state.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM investigation_tasks", [], |row| {
            row.get(0)
        })
        .unwrap()
}

// AC-0267: capture-only preparation creates no bootstrap legacy task, starts no
// auditor, resolves each role's exact mixed envelope and never calls a provider.
#[test]
fn completion_prepare_freezes_independent_mixed_requests_without_provider_or_legacy_task() {
    let directory = tempfile::tempdir().unwrap();
    let (provider, descriptor) = descriptor_only();
    let prepared = prepare(directory.path(), &descriptor, binding()).unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(task_count(&prepared.app_data), 1);
    assert!(prepared.auditor_id.is_none());
    assert_eq!(prepared.analyst_request.specialist_id, "domain-analyst@7");
    assert_eq!(prepared.auditor_request.specialist_id, "evidence-auditor@7");
    assert_ne!(
        prepared.analyst_request.intent_hash().unwrap(),
        prepared.auditor_request.intent_hash().unwrap()
    );
    for request in [&prepared.analyst_request, &prepared.auditor_request] {
        assert_eq!(request.schema_version, 4);
        assert_eq!(request.limit_profile, "investigation-v1");
        assert_eq!(request.obligations.schema_version, 1);
        assert_eq!(
            request.obligations.contract,
            "investigation-completion-obligations@1"
        );
        assert!(request.inspection_requirements.is_none());
        assert_eq!(request.obligations.obligations.len(), 2);
        assert_eq!(request.obligations.sources().unwrap().len(), 1);
        assert_eq!(
            request
                .obligations
                .obligations
                .iter()
                .filter(|item| matches!(item, Obligation::Inventory { .. }))
                .count(),
            1
        );
        assert!(!request.question.contains(&request.expected_context_id));
    }
    assert_eq!(
        prepared.auditor_request.parent_id.as_ref(),
        Some(&prepared.analyst_id)
    );
    assert_eq!(
        prepared.auditor_request.conversation_id.as_ref(),
        Some(&prepared.conversation_id)
    );
    let state = prepared.app.state::<AppState>();
    let jobs = state.jobs.lock().unwrap();
    let detail = jobs.investigation(&prepared.analyst_id).unwrap();
    assert_eq!(detail.summary.schema_version, 4);
    assert_eq!(detail.summary.status, InvestigationStatus::Queued);
    assert_eq!(detail.usage.model_invocations, 0);
    assert!(
        jobs.existing_investigation_completion(&prepared.auditor_request)
            .unwrap()
            .is_none()
    );
    let task: Vec<u8> = rusqlite::Connection::open(prepared.app_data.join("state.db"))
        .unwrap()
        .query_row("SELECT record FROM investigation_tasks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&task).unwrap()["schema_version"],
        2
    );
    let inventory: RepoCoverage =
        serde_json::from_slice(&read(&directory.path().join("inventory.json")).unwrap()).unwrap();
    assert!(inventory.limited);
    assert_eq!(
        inventory.producers[0].id,
        "harness-generated-synthetic-control@1"
    );
    assert!(
        inventory.producers[0]
            .stopping_reason
            .contains("not a full ingest-pipeline coverage claim")
    );
    assert_eq!(
        prepared.manifest.source_hash,
        content_hash(super::super::super::super::ORIGINAL.as_bytes())
    );
    assert_eq!(
        prepared.manifest.analyst_request_hash,
        prepared.analyst_request.intent_hash().unwrap()
    );
    assert_eq!(
        prepared.manifest.auditor_request_hash,
        prepared.auditor_request.intent_hash().unwrap()
    );
}

// AC-0267: fresh persisted state reopens without TempDir lifetime ownership,
// preserves actual terminal/queued tasks, and never recreates either role.
#[test]
fn completion_prepare_reopen_preserves_terminal_analyst_and_existing_auditor_without_replay() {
    let directory = tempfile::tempdir().unwrap();
    let (provider, descriptor) = descriptor_only();
    let prepared = prepare(directory.path(), &descriptor, binding()).unwrap();
    let frozen_hash = prepared.manifest_hash.clone();
    let graph_hash = prepared.graph_before_hash.clone();
    let analyst_request = serde_json::to_vec(&prepared.analyst_request).unwrap();
    let auditor_request = serde_json::to_vec(&prepared.auditor_request).unwrap();
    let state = prepared.app.state::<AppState>();
    let mut jobs = state.jobs.lock().unwrap();
    let analyst = jobs.investigation(&prepared.analyst_id).unwrap();
    let plan = jobs
        .claim_plan(analyst.summary.job_id, crate::jobs::ClaimMode::StartQueued)
        .unwrap();
    let reservation = state
        .job_execution_locks
        .try_reserve(plan.lock_target())
        .unwrap();
    let (_, execution) = jobs.claim_execution(&plan, reservation).unwrap();
    let revision = jobs
        .investigation(&prepared.analyst_id)
        .unwrap()
        .summary
        .revision;
    jobs.advance_investigation(
        &execution,
        revision,
        InvestigationTransition::StopWithElapsed {
            reason: InvestigationStopReason::Operational,
            active_milliseconds: 0,
        },
    )
    .unwrap();
    // Reopen is an evidence loader, not the analyst semantic gate. Simulate a
    // separately committed queued auditor to prove it is found, never recreated.
    let auditor = jobs
        .start_completion_investigation(&prepared.auditor_request, Some(&descriptor))
        .unwrap();
    let auditor_id = auditor.detail.summary.investigation_id;
    drop(jobs);
    drop(execution);
    drop(prepared);
    assert!(directory.path().join("fixture/private/state.db").is_file());
    let reopened = reopen(directory.path()).unwrap();
    assert_eq!(reopened.manifest_hash, frozen_hash);
    assert_eq!(reopened.graph_before_hash, graph_hash);
    assert_eq!(
        serde_json::to_vec(&reopened.analyst_request).unwrap(),
        analyst_request
    );
    assert_eq!(
        serde_json::to_vec(&reopened.auditor_request).unwrap(),
        auditor_request
    );
    assert_eq!(reopened.auditor_id.as_deref(), Some(auditor_id.as_str()));
    assert_eq!(task_count(&reopened.app_data), 2);
    assert_eq!(
        reopened
            .app
            .state::<AppState>()
            .jobs
            .lock()
            .unwrap()
            .investigation(&reopened.analyst_id)
            .unwrap()
            .summary
            .status,
        InvestigationStatus::Failed
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
}

// AC-0267: immutable artifacts and actual captured graph drift fail closed;
// failed preparation cannot silently overwrite the original frozen directory.
#[test]
fn completion_prepare_reopen_rejects_request_source_and_graph_drift() {
    for artifact in ["source-control.ts", "auditor-request.json", "graph"] {
        let directory = tempfile::tempdir().unwrap();
        let (_, descriptor) = descriptor_only();
        let prepared = prepare(directory.path(), &descriptor, binding()).unwrap();
        assert!(prepare(directory.path(), &descriptor, binding()).is_err());
        assert_eq!(task_count(&prepared.app_data), 1);
        if artifact == "graph" {
            let state = prepared.app.state::<AppState>();
            let mut graph = state.graph.lock().unwrap();
            let mut node = graph.read_snapshot().unwrap().0.remove(0);
            node.props["preparation_drift"] = true.into();
            graph.put_node(&node).unwrap();
        } else {
            let path = directory.path().join(artifact);
            let mut bytes = read(&path).unwrap();
            bytes.extend_from_slice(b" ");
            std::fs::write(path, bytes).unwrap();
        }
        drop(prepared);
        assert!(
            reopen(directory.path()).is_err(),
            "{artifact} drift must fail"
        );
    }
}
