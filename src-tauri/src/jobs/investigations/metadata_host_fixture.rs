//! Test-only seeding from the host's real frozen context and initialized stores.
//! The @9 start/input APIs seed the unchanged old family, then an explicit test
//! DTO conversion precedes a genuine immutable @10 sealed invocation insertion.
use super::*;
use crate::{job_execution::JobExecutionLocks, jobs::ClaimMode};

pub struct MetadataHostSeed {
    pub execution: JobExecution,
    pub detail: InvestigationDetail,
    pub request: StagedObligationRequest,
    pub metadata: ProtectedCompletionMetadata,
    pub step: PendingInvestigationStep,
}

pub fn seed(
    store: &mut JobStore,
    locks: &JobExecutionLocks,
    request: StagedObligationRequest,
    provider: &InvestigationProvider,
    ledger: &InvestigationInputLedger,
    archive: &InvestigationCoverageArchive,
    prepare_step: impl FnOnce(
        &InvestigationDetail,
        &StagedObligationRequest,
        &ProtectedCompletionMetadata,
    ) -> PendingInvestigationStep,
) -> MetadataHostSeed {
    let mut old_request = request.clone();
    old_request.schema_version = 6;
    old_request.specialist_id = "domain-analyst@9".into();
    let started = store
        .start_completion_investigation(&old_request, Some(provider))
        .unwrap();
    let plan = store
        .claim_plan(started.detail.summary.job_id, ClaimMode::StartQueued)
        .unwrap();
    let owner = locks.try_reserve(plan.lock_target()).unwrap();
    let (_, execution) = store.claim_execution(&plan, owner).unwrap();
    let id = started.detail.summary.investigation_id;
    let detail = store.investigation(&id).unwrap();
    store
        .advance_investigation(
            &execution,
            detail.summary.revision,
            InvestigationTransition::InputPrepared {
                coverage_archive: Some(Box::new(archive.clone())),
                ledger: Box::new(ledger.clone()),
                usage: detail.usage,
            },
        )
        .unwrap();
    let mut task = {
        let tx = store.conn.unchecked_transaction().unwrap();
        storage::load_task(&tx, &id).unwrap()
    };
    task.schema_version = 3;
    task.admission_state = Some(admission_contract::StoredAdmissionState {
        schema_version: 1,
        current_call: None,
    });
    task.detail.summary.schema_version = 7;
    task.detail.summary.specialist_id = SpecialistId::DomainAnalystV10;
    task.detail.specialist = SpecialistId::DomainAnalystV10.definition().unwrap();
    let request = request
        .normalized_with_policy(&INVESTIGATION_V2_POLICY)
        .unwrap();
    let intent = completion::host_fixture_intent(&request).unwrap();
    task.intent_hash = intent.request_hash.clone();
    task.detail.completion = Some(completion::initial_detail(&intent));
    task.completion_intent = Some(intent);
    let metadata = completion::derive_protected_metadata(&task, vec![], &[]).unwrap();
    let step = prepare_step(&task.detail, &request, &metadata);
    let tx = store.conn.transaction().unwrap();
    // The archive reader authenticates persisted family/intent authority.
    storage::save_next_prompt_task(&tx, &task, task.detail.summary.revision, false).unwrap();
    let basis = admission_contract::SavedCallBasis::from_pre_call_saved(
        &tx, &task, &step, ledger, &metadata,
    )
    .unwrap();
    insert_body(
        &tx,
        "investigation_invocations",
        &id,
        &step.step_id,
        &step,
        MAX_INVOCATION_BYTES,
    )
    .unwrap();
    task.phase = Phase::Invocation;
    task.pending = Some(step.clone());
    task.admission_state.as_mut().unwrap().current_call = Some(basis);
    task.detail.usage.model_invocations += 1;
    task.detail.usage.generated_token_reservations += step.generated_tokens;
    bump(&tx, &mut task).unwrap();
    event(
        &tx,
        &mut task,
        InvestigationEventKind::ModelStarted,
        Some(&step.step_id),
        None,
        "Test-only sealed host invocation; no provider executed.",
    )
    .unwrap();
    storage::save_next_prompt_task(&tx, &task, task.detail.summary.revision - 1, false).unwrap();
    tx.commit().unwrap();
    MetadataHostSeed {
        execution,
        detail: task.detail,
        request,
        metadata,
        step,
    }
}

pub fn prospective(
    store: &JobStore,
    id: &str,
    response: &InvestigationResponseMeta,
    ledger: &InvestigationInputLedger,
    tool: InvestigationTool,
) -> (InvestigationUsage, ProtectedCompletionMetadata) {
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = storage::load_next_prompt_task_base(&tx, id).unwrap();
    let (_, usage) = transitions::validate_response(&tx, &task, response).unwrap();
    let mut metadata = completion::metadata_current_next_prompt(&tx, &task).unwrap();
    if tool == InvestigationTool::ReadCoverageInventory {
        let read = &ledger.coverage_reads.last().unwrap().request;
        let intent = ReadIntent::Inventory {
            context_id: read.context_id.clone(),
            repo: read.repo.clone(),
            inventory_hash: read.inventory_hash.clone(),
            max_bytes: read.max_bytes,
        };
        let records = completion::derive_attempt_records(
            &task,
            &response.step_id,
            response,
            &intent,
            &AttemptOutcome::InventoryAcquired {
                read_index: ledger.coverage_reads.len() - 1,
            },
            Some(ledger),
            metadata.completed_attempt_ids.len(),
        )
        .unwrap();
        metadata
            .completed_attempt_ids
            .extend(records.into_iter().map(|record| record.attempt_id));
    }
    (usage, metadata)
}

pub fn inspect(
    store: &JobStore,
    id: &str,
) -> (InvestigationDetail, InvestigationInputLedger, bool) {
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = storage::load_next_prompt_task_base(&tx, id).unwrap();
    let ledger = storage::load_ledger(&tx, &task).unwrap().unwrap();
    (task.detail, ledger, task.phase == Phase::Ready)
}

pub fn cancel(store: &mut JobStore, id: &str) -> u64 {
    let tx = store.conn.transaction().unwrap();
    let mut task = storage::load_next_prompt_task_base(&tx, id).unwrap();
    let revision = task.detail.summary.revision;
    task.detail.summary.cancel_requested = true;
    bump(&tx, &mut task).unwrap();
    storage::save_next_prompt_task(&tx, &task, revision, false).unwrap();
    tx.commit().unwrap();
    task.detail.summary.revision
}

pub fn lose_next_ack() {
    super::metadata_admission::lose_next_commit_ack();
}
