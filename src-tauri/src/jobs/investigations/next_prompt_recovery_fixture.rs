//! Independent source data followed by genuine start/claim/initial/query/source
//! admission. No converted task rows, provider dispatch or source access.
use super::*;
use completion::InitialFixture;
use next_prompt_invocation::tests::{firewall, limits, provider};
use serde_json::json;

pub(super) fn source(charged: bool) -> InitialFixture {
    let (seed, target) = source_validation::internal_source_invocation_fixture();
    let mut selected = seed.ledger.clone();
    selected.revision = 1;
    let raw = json!({"context":{"context_id":seed.request.expected_context_id,
        "coverage":selected.coverage.as_ref().unwrap().qualification,
        "response":{"snapshot_id":selected.scope_snapshot_id,"view":"recovered_graph","total_selected":1,
        "facts":[{"reference":target.fact,"properties":{"note":"bounded native fixture"}}],"next_cursor":null}},
        "evidence_options":[{"fact":target.fact,"origin":"captured_primary_source_metadata","ranges":[]}]}).to_string();
    assert_eq!(
        core_prov::content_hash(raw.as_bytes()),
        selected.queries[0].response_hash
    );
    let tx = seed.store.conn.unchecked_transaction().unwrap();
    let archive = coverage::load_for_task(&tx, &seed.task).unwrap().unwrap();
    tx.commit().unwrap();
    let request = seed.request.clone();
    drop(seed);
    let directory = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(directory.path().join("state.db")).unwrap();
    let p = provider(false).0;
    let selected_provider = InvestigationProvider {
        mode: InvestigationProviderMode::Local,
        provider_id: p.provider_id,
        model: p.requested_model,
        endpoint: p.endpoint_id,
        deployment: None,
        protocol_version: Some(p.protocol_version),
        available: true,
        unavailable_reason: None,
    };
    let started = store
        .start_next_prompt_investigation(&request, Some(&selected_provider))
        .unwrap();
    let locks = JobExecutionLocks::open(directory.path(), store.execution_namespace()).unwrap();
    let plan = store
        .claim_plan(
            started.detail.summary.job_id,
            crate::jobs::ClaimMode::StartQueued,
        )
        .unwrap();
    let (_, execution) = store
        .claim_execution(&plan, locks.try_reserve(plan.lock_target()).unwrap())
        .unwrap();
    let tx = store.conn.unchecked_transaction().unwrap();
    let task = load_next_prompt_task_base(&tx, &started.detail.summary.investigation_id).unwrap();
    let metadata = completion::derive_protected_metadata(&task, vec![], &[]).unwrap();
    tx.commit().unwrap();
    let mut initial = selected.clone();
    initial.revision = 0;
    initial.queries.clear();
    initial.selected_facts.clear();
    initial.receipt_references.clear();
    initial.citations.clear();
    let mut f = InitialFixture {
        directory,
        store,
        execution,
        task,
        request,
        archive,
        ledger: initial,
        metadata,
    };
    let basis = InvestigationTurnBasis {
        input_ledger_hash: f
            .ledger
            .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
            .unwrap(),
        previous_ledger: None,
        completed_tool: None,
    };
    let current = build(
        &f,
        f.ledger.clone(),
        &[],
        &f.task.detail.usage,
        &basis,
        &f.metadata,
    );
    let prepared = seal(&f, &current, 1);
    f.store
        .admit_next_prompt_initial_context(
            &f.execution,
            f.task.detail.summary.revision,
            &NextPromptInvocation {
                input: &current,
                prepared: &prepared,
                active_milliseconds: f.task.detail.usage.active_milliseconds,
            },
            &f.archive,
            |_| Ok(()),
        )
        .unwrap();
    refresh(&mut f);
    begin(&mut f, &current, &prepared);
    let action = json!({"type":"query_context","context_id":f.request.expected_context_id,"query":selected.queries[0].query});
    let response = measured_response(&f, &action);
    let read = MetadataContinuationRead {
        request: &f.request,
        response: &response,
        original_action: &action,
        current: &current,
        current_prepared: &prepared,
        candidate_ledger: &selected,
        raw_response: &raw,
        kind: ContextPreviewKind::Query,
        active_milliseconds: response.active_milliseconds + 1,
    };
    let snapshot = f
        .store
        .investigation_metadata_continuation_snapshot(
            &f.task.detail.summary.investigation_id,
            f.task.detail.summary.revision,
            &read,
        )
        .unwrap();
    let candidate = build(
        &f,
        selected.clone(),
        &[raw.as_str()],
        snapshot.usage(),
        snapshot.basis(),
        snapshot.metadata(),
    );
    let next_prepared = seal(&f, &candidate, 2);
    let proof = current
        .certify_context_preview_v9_with_policy(
            &candidate,
            ContextPreviewKind::Query,
            &raw,
            snapshot.admitted_usage(),
            &provider(false).0,
            &limits(),
            snapshot.next_action_id(),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    f.store
        .admit_next_prompt_metadata(
            &f.execution,
            snapshot.revision(),
            &MetadataAdmission {
                response: &response,
                original_action: &action,
                current: &current,
                candidate: &candidate,
                current_prepared: &prepared,
                candidate_prepared: &next_prepared,
                proof: &proof,
                kind: ContextPreviewKind::Query,
                active_milliseconds: read.active_milliseconds,
            },
            &raw,
            |_| Ok(()),
        )
        .unwrap();
    f.ledger = selected;
    refresh(&mut f);
    begin(&mut f, &candidate, &next_prepared);
    let proof = candidate
        .certify_unread_source_v9_with_policy(
            &next_prepared,
            &target,
            next_prepared.profile(),
            next_prepared.limits(),
            &format!("{}:step-3", f.task.detail.summary.investigation_id),
            &INVESTIGATION_V2_POLICY,
        )
        .unwrap();
    let action =
        json!({"type":"read_evidence","fact":target.fact,"role":target.role,"index":target.index});
    let response = measured_response(&f, &action);
    let capability = f
        .store
        .admit_next_prompt_source(
            &f.execution,
            f.task.detail.summary.revision,
            &SourceAdmission {
                size_binding: None,
                response: &response,
                original_action: &action,
                current: &candidate,
                current_prepared: &next_prepared,
                proof: &proof,
                target: &target,
                active_milliseconds: response.active_milliseconds + 1,
            },
            |_| Ok(()),
        )
        .unwrap();
    if charged {
        f.store
            .charge_prepared_source_validation(
                &f.execution,
                &capability.basis(),
                &target,
                &f.ledger
                    .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)
                    .unwrap(),
            )
            .unwrap();
    }
    refresh(&mut f);
    f
}
fn build(
    f: &InitialFixture,
    ledger: InvestigationInputLedger,
    pages: &[&str],
    usage: &InvestigationUsage,
    basis: &InvestigationTurnBasis,
    metadata: &ProtectedCompletionMetadata,
) -> InvestigationInput {
    InvestigationInput::prepare_v9_with_policy(
        &f.request.question,
        f.task.detail.summary.specialist_id,
        ledger,
        pages,
        &[],
        &[],
        usage,
        basis,
        &f.request,
        metadata,
        &[],
        CompletionCallMode::Choice,
        0,
        None,
        &INVESTIGATION_V2_POLICY,
    )
    .unwrap()
}
fn seal(
    f: &InitialFixture,
    input: &InvestigationInput,
    step: u64,
) -> llm::bounded::PreparedBoundedCompletion {
    firewall()
        .prepare_bounded(
            &provider(false),
            &input
                .action(format!(
                    "{}:step-{step}",
                    f.task.detail.summary.investigation_id
                ))
                .unwrap(),
            &limits(),
        )
        .unwrap()
}
fn begin(
    f: &mut InitialFixture,
    input: &InvestigationInput,
    prepared: &llm::bounded::PreparedBoundedCompletion,
) {
    let authorized = firewall()
        .authorize_bounded(&provider(false), prepared, None)
        .unwrap();
    f.store
        .begin_next_prompt_invocation(
            &f.execution,
            f.task.detail.summary.revision,
            &NextPromptInvocation {
                input,
                prepared,
                active_milliseconds: f.task.detail.usage.active_milliseconds,
            },
            &authorized,
            |_| Ok(()),
        )
        .unwrap();
    refresh(f);
}
fn measured_response(f: &InitialFixture, action: &serde_json::Value) -> InvestigationResponseMeta {
    InvestigationResponseMeta {
        finish_feedback_witness: None,
        step_id: format!("step-{}", f.task.detail.usage.model_invocations),
        response_hash: core_prov::content_hash(b"complete recovery fixture response"),
        action_hash: core_prov::content_hash(&encode(action, MAX_ACTION_BYTES).unwrap()),
        observed_model: None,
        reported_input_tokens: Some(11),
        reported_output_tokens: Some(3),
        reported_cache_read_tokens: None,
        active_milliseconds: f.task.detail.usage.active_milliseconds + 1,
    }
}
pub(super) fn refresh(f: &mut InitialFixture) {
    let tx = f.store.conn.unchecked_transaction().unwrap();
    f.task = load_next_prompt_task(&tx, &f.task.detail.summary.investigation_id).unwrap();
}
