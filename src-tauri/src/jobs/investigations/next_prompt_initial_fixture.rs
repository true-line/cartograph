//! Explicit test-only conversion before any input/invocation rows exist. No
//! immutable history is deleted or rewritten and no production migration exists.
use super::*;
pub(in crate::jobs::investigations) struct InitialFixture {
    pub directory: tempfile::TempDir,
    pub store: JobStore,
    pub execution: JobExecution,
    pub task: StoredTask,
    pub request: StagedObligationRequest,
    pub archive: InvestigationCoverageArchive,
    pub ledger: InvestigationInputLedger,
    pub metadata: ProtectedCompletionMetadata,
}
pub(in crate::jobs::investigations) fn initial_fixture(
    mode: InvestigationProviderMode,
    provider: InvestigationProvider,
) -> InitialFixture {
    let mut fixture = Fixture::with_family_preparation(mode, &INVESTIGATION_V2_POLICY, true, false);
    let tx = fixture.store.conn.transaction().unwrap();
    let mut task = load_task(&tx, &fixture.id).unwrap();
    assert_eq!(task.phase, Phase::Preparing);
    assert!(task.ledger_revision.is_none());
    let mut request = fixture.request;
    request.schema_version = 7;
    request.specialist_id = "domain-analyst@10".into();
    let request = request
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    task.schema_version = 3;
    task.admission_state = Some(admission_contract::StoredAdmissionState {
        schema_version: 1,
        current_call: None,
    });
    task.detail.summary.schema_version = 7;
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV10;
    task.detail.specialist = SpecialistId::DomainAnalystV10.definition().unwrap();
    task.detail.provider = provider;
    let intent = CompletionIntent::new(&request).unwrap();
    task.intent_hash = intent.request_hash.clone();
    task.detail.completion = Some(initial_detail(&intent));
    task.completion_intent = Some(intent);
    let metadata = derive_protected_metadata(&task, vec![], &[]).unwrap();
    let bytes = encode(&task, MAX_RECORD_BYTES).unwrap();
    tx.execute(
        "UPDATE investigation_tasks SET record=?2,record_hash=?3 WHERE id=?1",
        params![fixture.id, bytes, core_prov::content_hash(&bytes)],
    )
    .unwrap();
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
        directory: fixture.directory,
        store: fixture.store,
        execution: fixture.execution,
        task,
        request,
        archive: fixture.archive,
        ledger,
        metadata,
    }
}
