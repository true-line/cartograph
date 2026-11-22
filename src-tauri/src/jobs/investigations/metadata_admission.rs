//! Exact metadata acquisitions published as one owner-authenticated transaction.
//! The caller retains PreparedContextRead guards until commit acknowledgement.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use serde_json::Value;

#[path = "metadata_continuation.rs"]
mod metadata_continuation;
pub use metadata_continuation::MetadataContinuationRead;
#[cfg(unix)]
pub(super) use metadata_continuation::MetadataContinuationSnapshot;
#[cfg(any(test, feature = "test-support"))]
pub(super) use metadata_continuation::tests::fixture::Proposal as MetadataProposalFixture;

pub struct MetadataAdmission<'a> {
    pub response: &'a InvestigationResponseMeta,
    pub original_action: &'a Value,
    pub current: &'a InvestigationInput,
    pub candidate: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
    pub candidate_prepared: &'a PreparedBoundedCompletion,
    pub proof: &'a ContextPreviewBound,
    pub kind: ContextPreviewKind,
    /// Host RunClock sample after deterministic preparation. The caller still
    /// checks its real deadline immediately before entering this writer.
    pub active_milliseconds: u64,
}

impl JobStore {
    pub fn admit_next_prompt_metadata(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        input: &MetadataAdmission<'_>,
        raw_response: &str,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let id = transitions::task_id_for_job(&tx, execution.id())?
            .ok_or(InvestigationStoreError::Missing)?;
        let task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != expected_revision || task.phase != Phase::Invocation {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let next = publish(&tx, task, input, raw_response, validate_retained)?;
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_AFTER_METADATA_COMMIT_ACK.with(|fault| fault.replace(false)) {
            // Test-only lost acknowledgement: the transaction did commit, but
            // the host must not install prepared state on the returned error.
            return Err(InvestigationStoreError::Storage);
        }
        Ok(next.detail)
    }
}

/// Shared current-call authentication; no writes, capacity permit or dispatch.
pub(super) struct AuthenticatedResponse {
    pub call: admission_contract::SavedCallBasis,
    pub core: admission_contract::SavedCallCore,
    pub admitted_usage: InvestigationUsage,
    pub original: Vec<u8>,
    pub next_action_id: String,
}
pub(super) fn authenticate_response(
    tx: &rusqlite::Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    original_action: &Value,
    current: &InvestigationInput,
    prepared: &PreparedBoundedCompletion,
    active_milliseconds: u64,
) -> Result<AuthenticatedResponse, InvestigationStoreError> {
    if response.finish_feedback_witness.is_some() {
        return Err(InvestigationStoreError::Invalid);
    }
    let authenticated = authenticate_saved_response(
        tx,
        task,
        response,
        original_action,
        current,
        prepared,
        active_milliseconds,
    )?;
    if authenticated.call.mode() != CompletionCallMode::Choice {
        return Err(InvestigationStoreError::Invalid);
    }
    if active_milliseconds
        >= task
            .detail
            .limits
            .active_seconds
            .checked_mul(1000)
            .ok_or(InvestigationStoreError::Capacity)?
    {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(authenticated)
}

/// Exact invoked input/response authentication shared by acquisition and finish.
/// Callers retain their action/mode and new-work deadline rules separately: a
/// valid late finish does not start another acquisition or provider invocation.
pub(super) fn authenticate_saved_response(
    tx: &rusqlite::Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    original_action: &Value,
    current: &InvestigationInput,
    prepared: &PreparedBoundedCompletion,
    active_milliseconds: u64,
) -> Result<AuthenticatedResponse, InvestigationStoreError> {
    if response.finish_telemetry.is_some() {
        return Err(InvestigationStoreError::Invalid);
    }
    let effective_policy = super::policy_for_detail(&task.detail)?;
    let policy = &effective_policy;
    let id = &task.detail.summary.investigation_id;
    // A saved call cannot make malformed earlier finish/refusal history usable
    // by a later acquisition. This replay uses the journal-independent core and
    // does not recurse through response authentication.
    completion::validate_saved(tx, task)?;
    let call = task
        .admission_state
        .as_ref()
        .and_then(|s| s.current_call.clone())
        .ok_or(InvestigationStoreError::Invalid)?;
    call.validate_saved(tx, task)?;
    let core = admission_contract::load_saved_call_basis_core(tx, task, &call)?;
    let (step, admitted_usage) = transitions::validate_response(tx, task, response)?;
    current.validate_response_metadata(response.observed_model.as_deref())?;
    let profile = prepared.profile();
    let provider = &task.detail.provider;
    if profile.provider_id != provider.provider_id
        || profile.requested_model != provider.model
        || profile.endpoint_id != provider.endpoint
        || provider.protocol_version.as_deref() != Some(profile.protocol_version.as_str())
        || (profile.locality == llm::Locality::Local)
            != (provider.mode == InvestigationProviderMode::Local)
        || provider.deployment.is_some()
        || active_milliseconds < admitted_usage.active_milliseconds
        || active_milliseconds < task.detail.usage.active_milliseconds
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let original = encode(original_action, MAX_ACTION_BYTES)?;
    if core_prov::content_hash(&original) != response.action_hash
        || current.ledger() != &core.ledger
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let current_action_id = format!("{id}:{}", call.step_id);
    let next_ordinal = admitted_usage
        .model_invocations
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    let next_action_id = format!("{id}:step-{next_ordinal}");
    if !prepared.matches_action(&current.action(current_action_id)?)
        || prepared.limits().values().max_output_tokens != step.generated_tokens
        || prepared.preview().egress.payload_hash != step.payload_hash
        || encode(
            &current
                .action(prepared.preview().egress.action_id.clone())?
                .payload,
            MAX_INPUT_BYTES,
        )?
        .len()
            != step.input_bytes
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let current_disclosure = current.seal_disclosure_with_policy(prepared, policy)?;
    if current_disclosure != step.disclosure {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(AuthenticatedResponse {
        call,
        core,
        admitted_usage,
        original,
        next_action_id,
    })
}

fn publish(
    tx: &rusqlite::Transaction<'_>,
    mut task: StoredTask,
    input: &MetadataAdmission<'_>,
    raw_response: &str,
    validate_retained: impl FnOnce(
        &crate::sources::RetainedSourceView<'_>,
    ) -> Result<(), InvestigationStoreError>,
) -> Result<StoredTask, InvestigationStoreError> {
    let effective_policy = super::policy_for_detail(&task.detail)?;
    let policy = &effective_policy;
    let expected_revision = task.detail.summary.revision;
    let id = task.detail.summary.investigation_id.clone();
    let authenticated = authenticate_response(
        tx,
        &task,
        input.response,
        input.original_action,
        input.current,
        input.current_prepared,
        input.active_milliseconds,
    )?;
    let metadata_continuation::DerivedContinuation {
        snapshot,
        tool,
        read,
        records,
    } = metadata_continuation::derive(
        tx,
        &task,
        input.response,
        &authenticated,
        input.candidate.ledger(),
        raw_response,
        input.kind,
        input.active_milliseconds,
    )?;
    let AuthenticatedResponse {
        call,
        core,
        admitted_usage,
        next_action_id,
        ..
    } = authenticated;
    if !input
        .candidate_prepared
        .matches_action(&input.candidate.action(next_action_id.clone())?)
        || input.current_prepared.profile() != input.candidate_prepared.profile()
        || input.current_prepared.limits() != input.candidate_prepared.limits()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let disclosure = input
        .candidate
        .seal_disclosure_with_policy(input.candidate_prepared, policy)?
        .ok_or(InvestigationStoreError::Invalid)?;
    input.proof.verify_with_policy(
        input.current,
        input.candidate,
        input.kind,
        raw_response,
        &admitted_usage,
        input.candidate_prepared.profile(),
        input.candidate_prepared.limits(),
        &next_action_id,
        policy,
    )?;
    let ledger = input.candidate.ledger();
    let metadata = snapshot.metadata();
    let usage = snapshot.usage().clone();
    let stamp = disclosure
        .admission
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if stamp.consumed_budgets != usage
        || stamp.capacity_feedback.is_some()
        || stamp.capacity_refusal_count != call.admission.capacity_refusal_count
        || disclosure.completion_metadata_hash.as_ref()
            != Some(&metadata.fingerprint_with_policy(policy)?)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if policy.local_finish_generation() {
        stamp.validate_mode_with_metadata(metadata, policy)?;
    } else if stamp.mode != snapshot.mode() {
        return Err(InvestigationStoreError::Invalid);
    }
    snapshot.validate_acquisition(stamp)?;
    task.detail.usage = usage.clone();
    task.action_tool = Some(tool);
    // Revalidate retained registry identities and already-held evidence guards
    // through this same state.db snapshot, before the first durable effect.
    // The host callback must not acquire source/registry/jobs locks anew.
    validate_retained(&crate::sources::RetainedSourceView::from_transaction(tx))?;
    insert_body(
        tx,
        "investigation_responses",
        &id,
        &call.step_id,
        input.response,
        4096,
    )?;
    bump(tx, &mut task)?;
    for (kind, summary) in [
        (
            InvestigationEventKind::ModelCompleted,
            "A complete provider response was recorded.",
        ),
        (
            InvestigationEventKind::ActionAdmitted,
            "The exact metadata acquisition passed continuation admission.",
        ),
        (
            InvestigationEventKind::ToolStarted,
            "The prepared metadata acquisition was admitted.",
        ),
    ] {
        event(
            tx,
            &mut task,
            kind,
            Some(&call.step_id),
            Some(tool),
            summary,
        )?;
    }
    transitions::persist_input_rows(tx, &task, ledger)?;
    task.ledger_revision = Some(ledger.revision);
    task.ledger_hash = Some(ledger.fingerprint_with_policy(policy)?);
    task.detail.citations = ledger.citations.clone();
    task.phase = Phase::Ready;
    task.pending = None;
    task.approved = false;
    task.action_tool = None;
    task.admission_state
        .as_mut()
        .ok_or(InvestigationStoreError::Invalid)?
        .current_call = None;
    bump(tx, &mut task)?;
    event(
        tx,
        &mut task,
        InvestigationEventKind::ToolCompleted,
        Some(&call.step_id),
        Some(tool),
        "Exact metadata, receipt references and completion observations committed together.",
    )?;
    if let Some(read) = &read {
        completion::append_next_prompt_attempt_at_step(
            tx,
            &task,
            &call,
            input.response,
            read,
            &AttemptOutcome::InventoryAcquired {
                read_index: core.ledger.coverage_reads.len(),
            },
            ledger,
            &records,
        )?;
    }
    completion::refresh_observation(tx, &mut task)?;
    admission_journal::derive_current(tx, &task)?;
    save_next_prompt_task(tx, &task, expected_revision, false)?;
    #[cfg(any(test, feature = "test-support"))]
    if FAIL_AFTER_METADATA_WRITE.with(|fault| fault.replace(false)) {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(task)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_AFTER_METADATA_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_AFTER_METADATA_COMMIT_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn lose_next_commit_ack() {
    FAIL_AFTER_METADATA_COMMIT_ACK.with(|fault| fault.set(true));
}

#[cfg(any(test, feature = "test-support"))]
#[path = "metadata_admission_tests.rs"]
mod tests;
