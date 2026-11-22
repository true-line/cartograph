//! Coherent, read-only post-read prompt inputs. Neither a source-access witness
//! nor permission to publish, dispatch, retry, or install the candidate.
use super::*;

pub enum SourceContinuationOutcome {
    Acquired,
    Unavailable(UnavailableReason),
}
pub struct SourceContinuationSnapshotInput<'a> {
    pub investigation_id: &'a str,
    pub expected_revision: u64,
    pub request: &'a StagedObligationRequest,
    pub candidate_ledger: &'a InvestigationInputLedger,
    pub outcome: SourceContinuationOutcome,
    pub active_milliseconds: u64,
}

/// Owned immutable projections from one saved read transaction. No conversion
/// to a settlement plan/capability exists; the writer rederives and reseals.
pub struct SourceContinuationSnapshot {
    saved_revision: u64,
    request: StagedObligationRequest,
    base_ledger: InvestigationInputLedger,
    ledger: InvestigationInputLedger,
    detail: InvestigationDetail,
    #[cfg(any(test, feature = "test-support"))]
    attempts: Vec<AttemptRecord>,
    #[cfg(any(test, feature = "test-support"))]
    attempt_ledgers: Vec<InvestigationInputLedger>,
    metadata: ProtectedCompletionMetadata,
    basis: InvestigationTurnBasis,
    mode: CompletionCallMode,
    capacity_refusal_count: u32,
    next_action_id: String,
    choices: Option<std::sync::Arc<agents::investigation::acquisition_choices::AcquisitionChoices>>,
    correction: Option<agents::investigation::acquisition_choices::AuthenticatedCitationCorrection>,
}
impl SourceContinuationSnapshot {
    pub fn attach_acquisition(
        &self,
        input: InvestigationInput,
    ) -> Result<InvestigationInput, InvestigationError> {
        match &self.choices {
            Some(choices) => {
                input.with_acquisition_correction(choices.clone(), None, self.correction.as_ref())
            }
            None => Ok(input),
        }
    }
    pub fn saved_revision(&self) -> u64 {
        self.saved_revision
    }
    pub fn request(&self) -> &StagedObligationRequest {
        &self.request
    }
    pub fn base_ledger(&self) -> &InvestigationInputLedger {
        &self.base_ledger
    }
    pub fn ledger(&self) -> &InvestigationInputLedger {
        &self.ledger
    }
    pub fn detail(&self) -> &InvestigationDetail {
        &self.detail
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn attempts(&self) -> &[AttemptRecord] {
        &self.attempts
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn attempt_ledgers(&self) -> &[InvestigationInputLedger] {
        &self.attempt_ledgers
    }
    pub fn metadata(&self) -> &ProtectedCompletionMetadata {
        &self.metadata
    }
    pub fn basis(&self) -> &InvestigationTurnBasis {
        &self.basis
    }
    pub fn mode(&self) -> CompletionCallMode {
        self.mode
    }
    pub fn capacity_refusal_count(&self) -> u32 {
        self.capacity_refusal_count
    }
    pub fn next_action_id(&self) -> &str {
        &self.next_action_id
    }
    // Every successful acquisition clears capacity_feedback. The count remains
    // the saved call's count, checked again by actual settlement validation.
    pub fn capacity_feedback(&self) -> Option<&AdmissionCapacityFeedback> {
        None
    }
}

/// Compare only independently derived authority, never a caller-supplied hash.
pub(super) fn validate_acquisition(
    choices: &agents::investigation::acquisition_choices::AcquisitionChoices,
    stamp: &DisclosedAdmissionState,
    policy: &InvestigationPolicy,
) -> Result<(), InvestigationStoreError> {
    stamp.validate_saved_role_shape(SpecialistId::DomainAnalystV18, policy)?;
    if stamp.acquisition.as_ref()
        != Some(&agents::investigation::AcquisitionAdmission {
            finish_patch: None,
            finish_policy_complete: policy.local_finish_generation() && choices.attempts_complete(),
            capacity_exhausted: false,
            finish_reason: None,
            choices_hash: choices.stamp_hash()?,
            action_feedback_hash: None,
        })
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}

impl JobStore {
    pub fn next_source_continuation_snapshot(
        &self,
        input: &SourceContinuationSnapshotInput<'_>,
    ) -> Result<SourceContinuationSnapshot, InvestigationStoreError> {
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task(&tx, input.investigation_id)?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let request = input.request.normalized_with_policy(policy)?;
        if task.phase != Phase::Reservation
            || task.detail.summary.revision != input.expected_revision
        {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if request.intent_hash_with_policy(policy)? != task.intent_hash {
            return Err(InvestigationStoreError::Conflict);
        }
        let saved = source_validation::validate_saved_pending(&tx, &task)?;
        let call = task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
            .ok_or(InvestigationStoreError::Invalid)?;
        let core = admission_contract::load_saved_call_basis_core(&tx, &task, call)?;
        let response: InvestigationResponseMeta = read_body(
            &tx,
            "investigation_responses",
            "step_id",
            input.investigation_id,
            &call.step_id,
        )?
        .ok_or(InvestigationStoreError::Missing)?;
        let (kind, reason) = match &input.outcome {
            SourceContinuationOutcome::Acquired => (DispositionKind::Acquired, None),
            SourceContinuationOutcome::Unavailable(reason) => {
                source_unavailable_reason(reason)?;
                (DispositionKind::ReadUnavailable, Some(reason.clone()))
            }
        };
        request.validate_ledger_with_policy(&core.ledger, policy)?;
        request.validate_ledger_with_policy(input.candidate_ledger, policy)?;
        validate_next_ledger(
            &task,
            &saved,
            &core.ledger,
            input.candidate_ledger,
            kind,
            input.active_milliseconds,
        )?;
        // This is the same production materialization used by the writer and
        // serializer envelope. It creates no SQL rows and publishes no evidence.
        let projected = materialize(
            &tx,
            &task,
            &saved,
            &response,
            &call.step_id,
            Some(input.candidate_ledger.clone()),
            kind,
            reason,
            input.active_milliseconds,
        )?;
        let suffix = projected
            .attempts
            .iter()
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>();
        let metadata = continuation_metadata(&core.metadata, &suffix)?;
        let choices = if task.detail.summary.specialist_id.uses_acquisition_choices() {
            completion::SavedMetadataHistory::load(&tx, &task)?.prospective_choices(
                input.candidate_ledger,
                &metadata,
                &suffix,
            )?
        } else {
            None
        };
        drop(suffix); // Metadata owns its result before the saved history reload.
        let mode = metadata.next_call_mode_with_policy(&projected.task.detail.usage, policy)?;
        let mut entries = completion::attempts(&tx, &task)?;
        let prior_len = entries.len();
        entries.extend(projected.attempts);
        let mut attempts = Vec::with_capacity(entries.len());
        #[cfg(any(test, feature = "test-support"))]
        let mut attempt_ledgers = Vec::with_capacity(entries.len());
        for (index, entry) in entries.into_iter().enumerate() {
            let ledger = if index >= prior_len {
                input.candidate_ledger.clone()
            } else {
                read_body::<InvestigationInputLedger>(
                    &tx,
                    "investigation_inputs",
                    "revision",
                    input.investigation_id,
                    &signed(entry.record.input_revision)?,
                )?
                .ok_or(InvestigationStoreError::Invalid)?
            };
            request.validate_ledger_with_policy(&ledger, policy)?;
            if ledger.revision != entry.record.input_revision
                || ledger.revision > input.candidate_ledger.revision
                || ledger.graph_snapshot_id != core.ledger.graph_snapshot_id
                || ledger.scope_snapshot_id != core.ledger.scope_snapshot_id
                || ledger.coverage != core.ledger.coverage
            {
                return Err(InvestigationStoreError::Invalid);
            }
            attempts.push(entry.record);
            #[cfg(any(test, feature = "test-support"))]
            attempt_ledgers.push(ledger);
        }
        if attempts
            .iter()
            .map(|record| &record.attempt_id)
            .ne(metadata.completed_attempt_ids.iter())
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let basis = if kind == DispositionKind::Acquired {
            InvestigationTurnBasis {
                input_ledger_hash: input.candidate_ledger.fingerprint_with_policy(policy)?,
                previous_ledger: Some(core.ledger.clone()),
                completed_tool: Some(InvestigationTool::ReadEvidence),
            }
        } else {
            turn::basis(&tx, &task)?
        };
        basis.validate_with_policy(input.candidate_ledger, policy)?;
        let result = SourceContinuationSnapshot {
            correction: completion::next_prompt_snapshot::authenticated_citation_correction(
                &tx, &task, &metadata,
            )?,
            saved_revision: task.detail.summary.revision,
            request,
            base_ledger: core.ledger,
            ledger: input.candidate_ledger.clone(),
            detail: projected.task.detail,
            #[cfg(any(test, feature = "test-support"))]
            attempts,
            #[cfg(any(test, feature = "test-support"))]
            attempt_ledgers,
            metadata,
            basis,
            mode,
            capacity_refusal_count: call.admission.capacity_refusal_count,
            choices,
            next_action_id: format!(
                "{}:step-{}",
                input.investigation_id,
                saved
                    .reservation
                    .binding
                    .base_usage
                    .model_invocations
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?
            ),
        };
        tx.commit()?;
        Ok(result)
    }
}

impl JobStore {
    /// Narrow live-owner publication route. Terminal/orphan classifications are
    /// deliberately absent from this interface.
    pub fn publish_next_prompt_source(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        continuation: SourceContinuation<'_>,
        outcome: SourceContinuationOutcome,
        active_milliseconds: u64,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        let outcome = match outcome {
            SourceContinuationOutcome::Acquired => SourceSettlement::Acquired(continuation),
            SourceContinuationOutcome::Unavailable(reason) => SourceSettlement::Unavailable {
                reason,
                continuation,
            },
        };
        self.settle_next_prompt_source(
            execution,
            revision,
            outcome,
            active_milliseconds,
            validate_retained,
        )
    }
}
