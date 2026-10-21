//! Internal coherent worker inputs, never a dispatch or acquisition capability.
use super::*;

/// The original normalized request is supplied by the live caller and checked
/// against the saved whole-request hash. It is not reconstructed from redacted
/// history. Transient payloads and execution ownership are deliberately absent.
pub(crate) struct NextPromptSnapshot {
    request: StagedObligationRequest,
    saved: CompletionPromptSnapshot,
    mode: CompletionCallMode,
    capacity: admission_journal::CapacityState,
    pending: Option<PendingInvestigationStep>,
}

impl NextPromptSnapshot {
    pub(crate) fn request(&self) -> &StagedObligationRequest {
        &self.request
    }
    pub(crate) fn detail(&self) -> &InvestigationDetail {
        self.saved.detail()
    }
    pub(crate) fn ledger(&self) -> &InvestigationInputLedger {
        self.saved.ledger()
    }
    pub(crate) fn attempts(&self) -> &[AttemptRecord] {
        self.saved.attempts()
    }
    pub(crate) fn attempt_ledgers(&self) -> &[InvestigationInputLedger] {
        self.saved.attempt_ledgers()
    }
    pub(crate) fn metadata(&self) -> &ProtectedCompletionMetadata {
        self.saved.metadata()
    }
    pub(crate) fn basis(&self) -> &InvestigationTurnBasis {
        self.saved.basis()
    }
    pub(crate) fn mode(&self) -> CompletionCallMode {
        self.mode
    }
    pub(crate) fn capacity_refusal_count(&self) -> u32 {
        self.capacity.count
    }
    pub(crate) fn capacity_feedback(&self) -> Option<&AdmissionCapacityFeedback> {
        self.capacity.feedback.as_ref()
    }
    /// Consent retains this exact sealed step; a worker must keep its matching
    /// live prepared payload instead of treating this metadata as a new grant.
    pub(crate) fn pending_step(&self) -> Option<&PendingInvestigationStep> {
        self.pending.as_ref()
    }
}

impl JobStore {
    /// One pinned saved-state snapshot for an internal Ready/Consent task3.
    /// No provider, public history/catalog route, or current graph is consulted.
    pub(crate) fn investigation_next_prompt_snapshot(
        &self,
        id: &str,
        request: &StagedObligationRequest,
    ) -> Result<NextPromptSnapshot, InvestigationStoreError> {
        let policy = &INVESTIGATION_V2_POLICY;
        let request = request.normalized_with_policy(policy)?;
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task(&tx, id)?;
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
        let derived_mode = metadata.next_call_mode_with_policy(&task.detail.usage, policy)?;
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
        let snapshot = NextPromptSnapshot {
            request,
            saved: CompletionPromptSnapshot {
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

#[cfg(test)]
#[path = "next_prompt_snapshot_tests.rs"]
pub(in crate::jobs::investigations) mod tests;
