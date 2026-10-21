//! Exact metadata acquisitions published as one owner-authenticated transaction.
//! The caller retains PreparedContextRead guards until commit acknowledgement.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use serde_json::Value;

pub(crate) struct MetadataAdmission<'a> {
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
    pub(crate) fn admit_next_prompt_metadata(
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
        #[cfg(test)]
        if FAIL_AFTER_METADATA_COMMIT_ACK.with(|fault| fault.replace(false)) {
            // Test-only lost acknowledgement: the transaction did commit, but
            // the host must not install prepared state on the returned error.
            return Err(InvestigationStoreError::Storage);
        }
        Ok(next.detail)
    }
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
    let policy = &INVESTIGATION_V2_POLICY;
    let expected_revision = task.detail.summary.revision;
    let id = task.detail.summary.investigation_id.clone();
    let call = task
        .admission_state
        .as_ref()
        .and_then(|s| s.current_call.clone())
        .ok_or(InvestigationStoreError::Invalid)?;
    if call.mode() != CompletionCallMode::Choice {
        return Err(InvestigationStoreError::Invalid);
    }
    call.validate_saved(tx, &task)?;
    let core = admission_contract::load_saved_call_basis_core(tx, &task, &call)?;
    let (step, admitted_usage) = transitions::validate_response(tx, &task, input.response)?;
    input
        .current
        .validate_response_metadata(input.response.observed_model.as_deref())?;
    let profile = input.current_prepared.profile();
    let provider = &task.detail.provider;
    if profile.provider_id != provider.provider_id
        || profile.requested_model != provider.model
        || profile.endpoint_id != provider.endpoint
        || provider.protocol_version.as_deref() != Some(profile.protocol_version.as_str())
        || (profile.locality == llm::Locality::Local)
            != (provider.mode == InvestigationProviderMode::Local)
        || provider.deployment.is_some()
        || input.active_milliseconds < admitted_usage.active_milliseconds
        || input.active_milliseconds < task.detail.usage.active_milliseconds
    {
        return Err(InvestigationStoreError::Invalid);
    }
    if input.active_milliseconds
        >= task
            .detail
            .limits
            .active_seconds
            .checked_mul(1000)
            .ok_or(InvestigationStoreError::Capacity)?
    {
        return Err(InvestigationStoreError::Capacity);
    }
    let original = encode(input.original_action, MAX_ACTION_BYTES)?;
    if core_prov::content_hash(&original) != input.response.action_hash
        || input.current.ledger() != &core.ledger
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let current_action_id = format!("{id}:{}", call.step_id);
    let next_ordinal = admitted_usage
        .model_invocations
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    let next_action_id = format!("{id}:step-{next_ordinal}");
    if !input
        .current_prepared
        .matches_action(&input.current.action(current_action_id)?)
        || !input
            .candidate_prepared
            .matches_action(&input.candidate.action(next_action_id.clone())?)
        || input.current_prepared.profile() != input.candidate_prepared.profile()
        || input.current_prepared.limits() != input.candidate_prepared.limits()
        || input.current_prepared.limits().values().max_output_tokens != step.generated_tokens
        || input.current_prepared.preview().egress.payload_hash != step.payload_hash
        || encode(
            &input
                .current
                .action(input.current_prepared.preview().egress.action_id.clone())?
                .payload,
            MAX_INPUT_BYTES,
        )?
        .len()
            != step.input_bytes
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let current_disclosure = input
        .current
        .seal_disclosure_with_policy(input.current_prepared, policy)?;
    if current_disclosure != step.disclosure {
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
    let context_id = &core
        .ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?
        .context_id;
    let action = ProgressiveInvestigationAction::decode_resolved(
        std::str::from_utf8(&original).map_err(|_| InvestigationStoreError::Invalid)?,
        context_id,
        &call.request_hash,
        &core.ledger,
        policy,
    )?;
    let (tool, read) = match (input.kind, action) {
        (
            ContextPreviewKind::Query,
            CompletionInvestigationAction::Read(QualifiedInvestigationAction::QueryContext {
                query,
                ..
            }),
        ) if ledger.queries.last().is_some_and(|m| m.query == query) => {
            (InvestigationTool::QueryContext, None)
        }
        (
            ContextPreviewKind::Inventory,
            CompletionInvestigationAction::Read(
                QualifiedInvestigationAction::ReadCoverageInventory(request),
            ),
        ) if ledger
            .coverage_reads
            .last()
            .is_some_and(|m| m.request == request) =>
        {
            (
                InvestigationTool::ReadCoverageInventory,
                Some(ReadIntent::Inventory {
                    context_id: request.context_id,
                    repo: request.repo,
                    inventory_hash: request.inventory_hash,
                    max_bytes: request.max_bytes,
                }),
            )
        }
        _ => return Err(InvestigationStoreError::Invalid),
    };
    let records = match &read {
        Some(read) => completion::derive_attempt_records(
            &task,
            &call.step_id,
            input.response,
            read,
            &AttemptOutcome::InventoryAcquired {
                read_index: core.ledger.coverage_reads.len(),
            },
            Some(ledger),
            core.metadata.completed_attempt_ids.len(),
        )?,
        None => vec![],
    };
    let mut metadata = core.metadata.clone();
    metadata
        .completed_attempt_ids
        .extend(records.iter().map(|r| r.attempt_id.clone()));
    let mut usage = admitted_usage;
    usage.active_milliseconds = input.active_milliseconds;
    usage.tool_actions = usage
        .tool_actions
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    usage.selected_facts = ledger.selected_facts.len();
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
    stamp.validate_with_metadata(&metadata, policy)?;
    task.detail.usage = usage.clone();
    task.action_tool = Some(tool);
    transitions::validate_input_candidate(tx, &task, ledger, &usage)?;
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
    #[cfg(test)]
    if FAIL_AFTER_METADATA_WRITE.with(|fault| fault.replace(false)) {
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(task)
}

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_METADATA_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_AFTER_METADATA_COMMIT_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn lose_next_commit_ack() {
    FAIL_AFTER_METADATA_COMMIT_ACK.with(|fault| fault.set(true));
}

#[cfg(test)]
#[path = "metadata_admission_tests.rs"]
mod tests;
