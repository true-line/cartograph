//! Genuine internal next-family start and execution claim. The legacy helper
//! supplies only independent request/archive fixture data, never task authority.
use super::*;
pub(in crate::jobs::investigations) struct InitialFixture {
    pub store: JobStore,
    pub execution: JobExecution,
    pub task: StoredTask,
    pub request: StagedObligationRequest,
    pub archive: InvestigationCoverageArchive,
    pub ledger: InvestigationInputLedger,
    pub metadata: ProtectedCompletionMetadata,
    /// Dropped last: Windows cannot remove it while a store holds files in it (#1307).
    pub directory: tempfile::TempDir,
}
pub(in crate::jobs::investigations) fn initial_fixture(
    mode: InvestigationProviderMode,
    provider: InvestigationProvider,
) -> InitialFixture {
    initial_fixture_with_request(mode, provider, |_, _| {})
}

pub(in crate::jobs::investigations) fn initial_fixture_with_request(
    mode: InvestigationProviderMode,
    provider: InvestigationProvider,
    customize: impl FnOnce(&mut StagedObligationRequest, &InvestigationCoverageArchive),
) -> InitialFixture {
    initial_fixture_with_configured_request(mode, provider, customize, None)
}

pub(in crate::jobs::investigations) fn initial_fixture_with_configured_request(
    mode: InvestigationProviderMode,
    provider: InvestigationProvider,
    customize: impl FnOnce(&mut StagedObligationRequest, &InvestigationCoverageArchive),
    active_seconds: Option<u64>,
) -> InitialFixture {
    let fixture = Fixture::with_family_preparation(mode, &INVESTIGATION_V2_POLICY, true, false);
    let mut request = fixture.request;
    request.schema_version = 7;
    request.specialist_id = "domain-analyst@10".into();
    customize(&mut request, &fixture.archive);
    let request = request
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let started = match active_seconds {
        Some(seconds) => store.start_configured_next_prompt_investigation(
            &request,
            Some(&provider),
            seconds,
            180_000,
        ),
        None => store.start_next_prompt_investigation(&request, Some(&provider)),
    }
    .unwrap();
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let plan = store
        .claim_plan(started.detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let reservation = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, reservation).unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &started.detail.summary.investigation_id).unwrap();
    assert_eq!(task.phase, Phase::Preparing);
    assert!(task.ledger_revision.is_none());
    let metadata = derive_protected_metadata(&task, vec![], &[]).unwrap();
    tx.commit().unwrap();
    let proof = fixture.archive.proof().clone();
    let ledger = InvestigationInputLedger {
        schema_version: 2,
        coverage: Some(proof.clone()),
        coverage_reads: vec![],
        graph_snapshot_id: proof.snapshot_id.clone(),
        scope_snapshot_id: proof.snapshot_id,
        revision: 0,
        selected_facts: vec![],
        receipt_references: vec![],
        citations: vec![],
        queries: vec![],
        supplied_history_hash: None,
        supplied_history_bytes: 0,
    };
    InitialFixture {
        directory,
        store,
        execution,
        task,
        request,
        archive: fixture.archive,
        ledger,
        metadata,
    }
}
