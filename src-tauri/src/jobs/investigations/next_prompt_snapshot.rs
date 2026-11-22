//! Internal coherent worker inputs, never a dispatch or acquisition capability.
use super::*;

/// The original normalized request is supplied by the live caller and checked
/// against the saved whole-request hash. It is not reconstructed from redacted
/// history. Transient payloads and execution ownership are deliberately absent.
pub struct NextPromptSnapshot {
    request: StagedObligationRequest,
    saved: CompletionPromptSnapshot,
    mode: CompletionCallMode,
    patch: Option<agents::investigation::finish_patch::PatchBasis>,
    capacity: admission_journal::CapacityState,
    pending: Option<PendingInvestigationStep>,
    choices: Option<std::sync::Arc<agents::investigation::acquisition_choices::AcquisitionChoices>>,
    correction: Option<agents::investigation::acquisition_choices::AuthenticatedCitationCorrection>,
}

impl NextPromptSnapshot {
    pub fn patch(&self) -> Option<&agents::investigation::finish_patch::PatchBasis> {
        self.patch.as_ref()
    }
    pub fn request(&self) -> &StagedObligationRequest {
        &self.request
    }
    pub fn detail(&self) -> &InvestigationDetail {
        self.saved.detail()
    }
    pub fn ledger(&self) -> &InvestigationInputLedger {
        self.saved.ledger()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn attempts(&self) -> &[AttemptRecord] {
        self.saved.attempts()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn attempt_ledgers(&self) -> &[InvestigationInputLedger] {
        self.saved.attempt_ledgers()
    }
    pub fn metadata(&self) -> &ProtectedCompletionMetadata {
        self.saved.metadata()
    }
    pub fn basis(&self) -> &InvestigationTurnBasis {
        self.saved.basis()
    }
    pub fn mode(&self) -> CompletionCallMode {
        self.mode
    }
    pub fn preparation_mode(&self) -> Result<CompletionCallMode, InvestigationStoreError> {
        let policy = super::policy_for_detail(self.detail())?;
        Ok(self
            .metadata()
            .next_call_mode_with_policy(&self.detail().usage, &policy)?)
    }
    pub fn capacity_refusal_count(&self) -> u32 {
        self.capacity.count
    }
    pub fn capacity_feedback(&self) -> Option<&AdmissionCapacityFeedback> {
        self.capacity.feedback.as_ref()
    }
    pub fn attach_acquisition(
        &self,
        input: InvestigationInput,
    ) -> Result<InvestigationInput, InvestigationStoreError> {
        let input = match &self.choices {
            Some(choices) => input.with_acquisition_correction(
                choices.clone(),
                self.capacity.action_feedback.clone(),
                self.correction.as_ref(),
            )?,
            None => input,
        };
        Ok(match &self.patch {
            Some(patch) => input.with_finish_patch(patch)?,
            None => input,
        })
    }
    /// Consent retains this exact sealed step and its matching prepared payload.
    pub fn pending_step(&self) -> Option<&PendingInvestigationStep> {
        self.pending.as_ref()
    }
}

impl JobStore {
    /// One pinned saved-state snapshot for an internal Ready/Consent task3.
    /// No provider, public history/catalog route, or current graph is consulted.
    pub fn investigation_next_prompt_snapshot(
        &self,
        id: &str,
        request: &StagedObligationRequest,
    ) -> Result<NextPromptSnapshot, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task(&tx, id)?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let request = request.normalized_with_policy(policy)?;
        if !matches!(task.phase, Phase::Ready | Phase::Consent) {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if request.intent_hash_with_policy(policy)? != task.intent_hash {
            return Err(InvestigationStoreError::Conflict);
        }
        let ledger = load_ledger(&tx, &task)?.ok_or(InvestigationStoreError::Missing)?;
        request.validate_ledger_with_policy(&ledger, policy)?;
        let entries = attempts(&tx, &task)?;
        let attempt_ledgers = entries
            .iter()
            .map(|entry| {
                let historical = ledger_at(&tx, id, entry.record.input_revision)?;
                request.validate_ledger_with_policy(&historical, policy)?;
                if historical.revision > ledger.revision
                    || historical.graph_snapshot_id != ledger.graph_snapshot_id
                    || historical.scope_snapshot_id != ledger.scope_snapshot_id
                    || historical.coverage != ledger.coverage
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                Ok(historical)
            })
            .collect::<Result<Vec<_>, InvestigationStoreError>>()?;
        let metadata = metadata_current_next_prompt(&tx, &task)?;
        let basis = super::super::turn::basis(&tx, &task)?;
        let capacity = admission_journal::derive_current(&tx, &task)?;
        let choices = if task.detail.summary.specialist_id.uses_acquisition_choices() {
            completion::SavedMetadataHistory::load(&tx, &task)?
                .current_choices(&ledger, &metadata)?
        } else {
            None
        };
        let patch = super::super::volatile_patch::authenticate(&tx, &task)?;
        let derived_mode = if patch.is_some()
            || choices.as_ref().is_some_and(|choices| {
                choices.capacity_exhausted()
                    || (policy.local_finish_generation() && choices.attempts_complete())
            })
            || capacity.action_feedback.as_ref().is_some_and(|feedback| {
                feedback.reason == agents::investigation::ActionRefusalReason::RepeatedRead
            }) {
            CompletionCallMode::FinishOnly
        } else {
            metadata.next_call_mode_with_policy(&task.detail.usage, policy)?
        };
        let pending = task.pending.clone();
        let mode = if task.phase == Phase::Consent {
            let call = task
                .admission_state
                .as_ref()
                .and_then(|state| state.current_call.as_ref())
                .ok_or(InvestigationStoreError::Invalid)?;
            call.validate_pending_consent(&tx, &task)?;
            if call.mode() != derived_mode
                || call.admission.consumed_budgets != task.detail.usage
                || call.admission.capacity_refusal_count != capacity.count
                || call.admission.capacity_feedback != capacity.feedback
            {
                return Err(InvestigationStoreError::Invalid);
            }
            call.mode()
        } else {
            derived_mode
        };
        let correction = authenticated_citation_correction(&tx, &task, &metadata)?;
        let snapshot = NextPromptSnapshot {
            patch,
            correction,
            request,
            choices,
            saved: CompletionPromptSnapshot {
                query_refusal: super::super::query_refusal::latest(&tx, &task)?,
                detail: task.detail,
                ledger,
                attempts: entries.into_iter().map(|entry| entry.record).collect(),
                attempt_ledgers,
                metadata,
                basis,
            },
            mode,
            capacity,
            pending,
        };
        tx.commit()?;
        Ok(snapshot)
    }
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_prompt_snapshot_tests.rs"]
pub(in crate::jobs::investigations) mod tests;

/// Load authenticated immutable refusal evidence before projecting new actions.
/// The supplied metadata may include prospective attempts, but its refusal
/// identity and complete feedback must still match the durable journal.
pub(in crate::jobs::investigations) fn authenticated_citation_correction(
    connection: &Connection,
    task: &StoredTask,
    metadata: &ProtectedCompletionMetadata,
) -> Result<
    Option<agents::investigation::acquisition_choices::AuthenticatedCitationCorrection>,
    InvestigationStoreError,
> {
    let Some(correction) = metadata
        .feedback
        .as_ref()
        .and_then(|feedback| feedback.citation_correction.as_ref())
    else {
        return Ok(None);
    };
    if !task
        .detail
        .summary
        .specialist_id
        .uses_typed_citation_correction()
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let entries = staged::load_for_task(connection, task)?;
    let entry = entries.last().ok_or(InvestigationStoreError::Invalid)?;
    if metadata.last_refusal_id.as_deref() != Some(entry.record.refusal_id.as_str())
        || metadata.refusal_count != entry.record.refusal_count
        || metadata.feedback.as_ref() != Some(&entry.record.feedback)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let basis = admission_journal::AuthenticatedRefusalBasis::load(
        connection,
        task,
        entry.record.refusal_count,
        entry,
    )?
    .ok_or(InvestigationStoreError::Invalid)?;
    Ok(Some(
        basis
            .binding()?
            .authenticate_stored_citation_correction(correction)?,
    ))
}
