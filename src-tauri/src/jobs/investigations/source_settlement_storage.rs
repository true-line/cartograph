//! Private source settlement storage kernel.
use super::*;

/// Consume the exact private actual-outcome projection in the caller's
/// authenticated transaction. No commit/ack or owner proof is fabricated here.
/// Its own allocation is released by the disposition before capacity checks;
/// every other task's pending allocations remain charged throughout.
pub(in crate::jobs::investigations) fn persist(
    tx: &rusqlite::Transaction<'_>,
    before: &StoredTask,
    plan: SettlementPlan,
) -> Result<StoredTask, InvestigationStoreError> {
    let id = &before.detail.summary.investigation_id;
    let actual = load_next_prompt_task_base(tx, id)?;
    if encode(&actual, MAX_RECORD_BYTES)? != encode(before, MAX_RECORD_BYTES)?
        || plan.before_hash != core_prov::content_hash(&encode(before, MAX_RECORD_BYTES)?)
    {
        return Err(InvestigationStoreError::Stale);
    }
    let saved = source_validation::validate_saved_pending(tx, before)?;
    let claim = &saved.reservation.storage_claim;
    let p = plan.projection;
    p.disposition
        .validate(&saved.reservation, saved.charge.as_ref())?;
    if p.additional_bytes > claim.settlement_bytes
        || p.counts.events as usize > claim.settlement_events
        || p.counts.attempts > claim.settlement_attempts
        || p.counts.input_rows > 1
        || p.counts.reference_rows != 0
        || p.counts.disposition_rows != 1
    {
        return Err(InvestigationStoreError::Capacity);
    }
    let task_before = actual_stored_bytes(tx, Some(id))?;
    let global_before = actual_stored_bytes(tx, None)?;
    read_reservations::ReadTransaction::new(tx)?.insert_disposition(
        id,
        &saved.reservation.binding.step_id,
        &p.disposition,
    )?;
    for event in &p.events {
        persist_event(tx, event)?;
    }
    if let Some(ledger) = &p.ledger {
        transitions::persist_input_rows(tx, &p.task, ledger)?;
    }
    completion::persist_projected_entries(tx, id, p.attempt_offset, &p.attempts)?;
    // Refusal rows are measured apart from the reservation's settlement claim
    // and bounded by the task and store capacity below, as in slice 2a.
    let mut refusal_bytes = 0;
    if let Some(refusal) = &p.refusal {
        let before_refusal = actual_stored_bytes(tx, Some(id))?;
        persist_event(tx, &refusal.event)?;
        completion::read_refusals::insert_planned_observations(tx, id, &refusal.planned)?;
        completion::persist_projected_entries(
            tx,
            id,
            p.attempt_offset + p.attempts.len(),
            &refusal.entries,
        )?;
        refusal_bytes = actual_stored_bytes(tx, Some(id))?.saturating_sub(before_refusal);
    }
    if p.task.phase == Phase::Terminal {
        transitions::sync_terminal_job(tx, &p.task)?;
    }
    admission_journal::derive_current(tx, &p.task)?;
    // Re-reading the actual published rows uses the historical validators and
    // must produce exactly the same task observation as the pure projection.
    let mut observed = p.task.clone();
    completion::refresh_observation(tx, &mut observed)?;
    if encode(&observed, MAX_RECORD_BYTES)? != encode(&p.task, MAX_RECORD_BYTES)? {
        return Err(InvestigationStoreError::Invalid);
    }
    save_next_prompt_task(
        tx,
        &p.task,
        before.detail.summary.revision,
        p.task.phase == Phase::Terminal,
    )?;
    for (before, after) in [
        (task_before, actual_stored_bytes(tx, Some(id))?),
        (global_before, actual_stored_bytes(tx, None)?),
    ] {
        if after.saturating_sub(before).saturating_sub(refusal_bytes) > claim.settlement_bytes {
            return Err(InvestigationStoreError::Capacity);
        }
    }
    if p.refusal.is_some()
        && (actual_stored_bytes(tx, Some(id))? > TASK_CAPACITY - TERMINAL_RESERVE
            || actual_stored_bytes(tx, None)? > STORE_CAPACITY)
    {
        return Err(InvestigationStoreError::Capacity);
    }
    #[cfg(test)]
    inject_failure(SettlementFailure::AfterTaskSave)?;
    Ok(p.task)
}

impl JobStore {
    /// Dispose a known host outcome while retaining the original acknowledged
    /// reservation identity. Cancellation/charge revision advances are allowed;
    /// an already published continuation is never overwritten after lost ACK.
    pub(crate) fn discard_next_prompt_source(
        &mut self,
        execution: &JobExecution,
        reservation: &CommittedSourceReservation,
        outcome: KnownSourceDisposition,
        active_milliseconds: u64,
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
        let before = load_next_prompt_task(&tx, &id)?;
        transitions::check_identity(&before, execution)?;
        transitions::check_job(&tx, execution, true)?;
        if before.phase != Phase::Reservation || before.pending_source_read.is_none() {
            return Err(InvestigationStoreError::Stale);
        }
        let saved = source_validation::validate_saved_pending(&tx, &before)?;
        if !reservation.matches_saved(&saved.reservation)? {
            return Err(InvestigationStoreError::Conflict);
        }
        // A lost charge acknowledgement still leaves its immutable charge here.
        // A known read cannot have occurred without that durable access fence.
        if !matches!(outcome, KnownSourceDisposition::ReleasedBeforeRead) && saved.charge.is_none()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let kind = match outcome {
            KnownSourceDisposition::ReleasedBeforeRead => DispositionKind::ReleasedBeforeRead,
            KnownSourceDisposition::KnownReadDiscarded => DispositionKind::KnownReadDiscarded,
            KnownSourceDisposition::BoundInvariantDefect => DispositionKind::BoundInvariantDefect,
        };
        let plan = project(
            &tx,
            &before,
            SourceSettlement::Terminal(kind),
            active_milliseconds,
        )?;
        let task = persist(&tx, &before, plan)?;
        tx.commit()?;
        #[cfg(test)]
        inject_failure(SettlementFailure::AfterCommitAck)?;
        Ok(task.detail)
    }
    /// Private live-owner settlement. The adapter supplies truthful access-stage
    /// classification and retains its source guards through acknowledgement.
    /// Unknown outcomes cannot be converted into live-owner retries here.
    pub(in crate::jobs::investigations) fn settle_next_prompt_source(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        outcome: SourceSettlement<'_>,
        active_milliseconds: u64,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        if matches!(
            outcome,
            SourceSettlement::Terminal(DispositionKind::OutcomeUnknown)
        ) {
            return Err(InvestigationStoreError::Invalid);
        }
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
        let before = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&before, execution)?;
        transitions::check_job(
            &tx,
            execution,
            matches!(outcome, SourceSettlement::Terminal(_)),
        )?;
        if before.detail.summary.revision != expected_revision {
            return Err(InvestigationStoreError::Stale);
        }
        let plan = project(&tx, &before, outcome, active_milliseconds)?;
        // Terminal discard/release can follow source invalidation. Acquired and
        // genuine unavailable continuations require retained identities still
        // valid in this exact state.db snapshot before any durable publication.
        if !plan.terminal() {
            validate_retained(&crate::sources::RetainedSourceView::from_transaction(&tx))?;
        }
        let task = persist(&tx, &before, plan)?;
        tx.commit()?;
        #[cfg(test)]
        inject_failure(SettlementFailure::AfterCommitAck)?;
        Ok(task.detail)
    }

    /// A later writer holds the actual recovered OS lock. This releases only
    /// live/storage slots: the disposition retains reserved-unknown tool and
    /// evidence accounting and any genuine validation charge forever.
    pub(in crate::jobs::investigations) fn settle_orphaned_next_prompt_source(
        &mut self,
        candidate: &InvestigationRecoveryCandidate,
        reservation: ExecutionReservation,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        reservation.verify()?;
        if candidate.target.namespace != self.namespace
            || reservation.target.namespace != self.namespace
            || candidate.target.id != reservation.target.id
            || !candidate.target.existing
            || !reservation.target.existing
        {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task_base(&tx, &candidate.id)?;
        if task.detail.summary.revision != candidate.revision
            || task.execution != candidate.identity
            || task.detail.summary.job_id != candidate.target.id
        {
            return Err(InvestigationStoreError::Stale);
        }
        let plan = project(
            &tx,
            &task,
            SourceSettlement::Terminal(DispositionKind::OutcomeUnknown),
            task.detail.usage.active_milliseconds,
        )?;
        let next = persist(&tx, &task, plan)?;
        tx.commit()?;
        #[cfg(test)]
        inject_failure(SettlementFailure::AfterCommitAck)?;
        Ok(next.detail)
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SettlementFailure {
    AfterTaskSave,
    AfterCommitAck,
}
#[cfg(test)]
thread_local! { static SETTLEMENT_FAILURE:std::cell::Cell<Option<SettlementFailure>>=const {std::cell::Cell::new(None)}; }
#[cfg(test)]
pub(super) fn fail_once(at: SettlementFailure) {
    SETTLEMENT_FAILURE.with(|slot| slot.set(Some(at)));
}
#[cfg(test)]
fn inject_failure(at: SettlementFailure) -> Result<(), InvestigationStoreError> {
    SETTLEMENT_FAILURE.with(|slot| {
        if slot.get() == Some(at) {
            slot.set(None);
            Err(InvestigationStoreError::Storage)
        } else {
            Ok(())
        }
    })
}
