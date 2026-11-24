//! One read-only replay epoch, never cross-transaction cached authority.
use super::*;
use std::collections::BTreeMap;

pub(in super::super) struct SavedMetadataHistory<'a> {
    connection: &'a Connection,
    task: &'a StoredTask,
    changes: u64,
    schema_version: i64,
    temp_schema_version: i64,
    steps: Vec<(u32, PendingInvestigationStep)>,
    ordinals: BTreeMap<String, u32>,
    attempts: Vec<AttemptEntry>,
    refusals: Vec<staged::JournalEntry>,
    archive: Option<InvestigationCoverageArchive>,
}
impl<'a> SavedMetadataHistory<'a> {
    pub(in super::super) fn load(
        connection: &'a Connection,
        task: &'a StoredTask,
    ) -> Result<Self, InvestigationStoreError> {
        if connection.is_autocommit() {
            return Err(InvestigationStoreError::Invalid);
        }
        let changes = connection.total_changes();
        let schema_version = connection.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
        let temp_schema_version =
            connection.query_row("PRAGMA temp.schema_version", [], |r| r.get(0))?;
        let steps = raw_next_prompt_invocations(connection, task)?;
        let ordinals = steps
            .iter()
            .map(|(ordinal, step)| (step.step_id.clone(), *ordinal))
            .collect();
        let value = Self {
            connection,
            task,
            changes,
            schema_version,
            temp_schema_version,
            steps,
            ordinals,
            attempts: attempts(connection, task)?,
            refusals: raw_refusals(connection, task)?,
            archive: coverage::load_for_task(connection, task)?,
        };
        // Check all references, even when the selected cutoff is earlier.
        for entry in &value.attempts {
            value.ordinal(&entry.record.step_id)?;
        }
        for entry in &value.refusals {
            value.ordinal(&entry.record.step_id)?;
        }
        value.check_epoch()?;
        Ok(value)
    }
    fn check_epoch(&self) -> Result<(), InvestigationStoreError> {
        if self.connection.is_autocommit()
            || self.connection.total_changes() != self.changes
            || self
                .connection
                .query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))?
                != self.schema_version
            || self
                .connection
                .query_row("PRAGMA temp.schema_version", [], |r| r.get::<_, i64>(0))?
                != self.temp_schema_version
        {
            return Err(InvestigationStoreError::Stale);
        }
        Ok(())
    }
    fn ordinal(&self, step: &str) -> Result<u32, InvestigationStoreError> {
        self.ordinals
            .get(step)
            .copied()
            .ok_or(InvestigationStoreError::Missing)
    }
    pub(in super::super) fn step(
        &self,
        step: &str,
    ) -> Result<&PendingInvestigationStep, InvestigationStoreError> {
        self.check_epoch()?;
        let ordinal = self.ordinal(step)?;
        self.steps
            .get((ordinal - 1) as usize)
            .map(|(_, step)| step)
            .ok_or(InvestigationStoreError::Missing)
    }
    pub(in super::super) fn metadata_at(
        &self,
        step: &str,
    ) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
        self.check_epoch()?;
        self.metadata_before(self.ordinal(step)?)
    }
    fn metadata_before(
        &self,
        cutoff: u32,
    ) -> Result<ProtectedCompletionMetadata, InvestigationStoreError> {
        let records = self
            .attempts
            .iter()
            .filter(|entry| self.ordinals[&entry.record.step_id] < cutoff)
            .map(|entry| entry.record.attempt_id.clone())
            .collect();
        let refusals = self
            .refusals
            .iter()
            .filter(|entry| self.ordinals[&entry.record.step_id] < cutoff)
            .map(|entry| &entry.record)
            .collect::<Vec<_>>();
        derive_protected_metadata(self.task, records, &refusals)
    }
    pub(in super::super) fn acquisition_choices_at(
        &self,
        basis: &admission_contract::SavedCallBasis,
        ledger: &InvestigationInputLedger,
    ) -> Result<
        Option<agents::investigation::acquisition_choices::AcquisitionChoices>,
        InvestigationStoreError,
    > {
        self.check_epoch()?;
        if !self
            .task
            .detail
            .summary
            .specialist_id
            .uses_acquisition_choices()
        {
            return Ok(None);
        }
        let cutoff = self.ordinal(&basis.step_id)?;
        if basis.task_id != self.task.detail.summary.investigation_id
            || basis.invocation_ordinal != cutoff
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let choices =
            self.choices_before(cutoff, ledger, &self.metadata_at(&basis.step_id)?, &[])?;
        let patch = basis
            .admission
            .acquisition
            .as_ref()
            .and_then(|s| s.finish_patch.as_ref());
        if let Some(patch) = patch {
            if !self.task.detail.summary.specialist_id.uses_finish_patch() {
                return Err(InvestigationStoreError::Invalid);
            }
            let prior = self
                .refusals
                .iter()
                .filter(|entry| self.ordinals[&entry.record.step_id] < cutoff)
                .collect::<Vec<_>>();
            let [prior] = prior.as_slice() else {
                return Err(InvestigationStoreError::Invalid);
            };
            if agents::investigation::finish_patch::PatchBasis::from_refusal_for_role(
                &prior.record,
                patch.finding_count,
                self.task.detail.summary.specialist_id,
            )
            .map_err(|_| InvestigationStoreError::Invalid)?
                != *patch
            {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        if basis
            .admission
            .acquisition
            .as_ref()
            .map(|stamp| stamp.choices_hash.as_str())
            != Some(choices.stamp_hash()?.as_str())
            || basis.admission.acquisition.as_ref().is_some_and(|stamp| {
                stamp.capacity_exhausted != choices.capacity_exhausted()
                    || stamp.finish_policy_complete
                        != (policy_for_detail(&self.task.detail)
                            .is_ok_and(|policy| policy.local_finish_generation())
                            && (choices.attempts_complete() || patch.is_some()))
            })
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(Some(choices))
    }

    pub(in super::super) fn current_choices(
        &self,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
    ) -> Result<
        Option<std::sync::Arc<agents::investigation::acquisition_choices::AcquisitionChoices>>,
        InvestigationStoreError,
    > {
        self.check_epoch()?;
        if !self
            .task
            .detail
            .summary
            .specialist_id
            .uses_acquisition_choices()
        {
            return Ok(None);
        }
        let cutoff = self
            .task
            .detail
            .usage
            .model_invocations
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        if self.metadata_before(cutoff)? != *metadata {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(Some(std::sync::Arc::new(self.choices_before(
            cutoff,
            ledger,
            metadata,
            &[],
        )?)))
    }
    /// The caller must independently derive this suffix from the actual read
    /// outcome; this read-only projection grants no publication authority.
    pub(in super::super) fn prospective_choices(
        &self,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
        suffix: &[AttemptRecord],
    ) -> Result<
        Option<std::sync::Arc<agents::investigation::acquisition_choices::AcquisitionChoices>>,
        InvestigationStoreError,
    > {
        self.check_epoch()?;
        if !self
            .task
            .detail
            .summary
            .specialist_id
            .uses_acquisition_choices()
        {
            return Ok(None);
        }
        let cutoff = self
            .task
            .detail
            .usage
            .model_invocations
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        let mut expected = self.metadata_before(cutoff)?;
        if expected
            .completed_attempt_ids
            .len()
            .checked_add(suffix.len())
            .is_none_or(|count| count > INVESTIGATION_V2_POLICY.max_completed_attempts())
        {
            return Err(InvestigationStoreError::Capacity);
        }
        let call = self
            .task
            .admission_state
            .as_ref()
            .and_then(|state| state.current_call.as_ref())
            .ok_or(InvestigationStoreError::Invalid)?;
        if suffix.iter().any(|record| {
            record.step_id != call.step_id || record.input_revision != ledger.revision
        }) {
            return Err(InvestigationStoreError::Invalid);
        }
        expected
            .completed_attempt_ids
            .extend(suffix.iter().map(|record| record.attempt_id.clone()));
        if expected != *metadata {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(Some(std::sync::Arc::new(
            self.choices_before(cutoff, ledger, metadata, suffix)?,
        )))
    }
    fn choices_before(
        &self,
        cutoff: u32,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
        suffix: &[AttemptRecord],
    ) -> Result<
        agents::investigation::acquisition_choices::AcquisitionChoices,
        InvestigationStoreError,
    > {
        let records = self
            .attempts
            .iter()
            .filter(|entry| self.ordinals[&entry.record.step_id] < cutoff)
            .map(|entry| &entry.record)
            .collect::<Vec<_>>();
        let mut ledgers = BTreeMap::new();
        for entry in &records {
            let revision = entry.input_revision;
            if let std::collections::btree_map::Entry::Vacant(slot) = ledgers.entry(revision) {
                let value = ledger_at(
                    self.connection,
                    &self.task.detail.summary.investigation_id,
                    revision,
                )?;
                if value.revision != revision
                    || value.coverage.as_ref()
                        != self.archive.as_ref().map(|archive| archive.proof())
                {
                    return Err(InvestigationStoreError::Invalid);
                }
                slot.insert(value);
            }
        }
        let evidence = records
            .iter()
            .map(
                |entry| agents::investigation::staged_attempt::AttemptEvidence {
                    record: entry,
                    ledger: &ledgers[&entry.input_revision],
                },
            )
            .chain(suffix.iter().map(|record| {
                agents::investigation::staged_attempt::AttemptEvidence { record, ledger }
            }))
            .collect::<Vec<_>>();
        let choices = agents::investigation::acquisition_choices::AcquisitionChoices::derive(
            &self
                .task
                .completion_intent
                .as_ref()
                .ok_or(InvestigationStoreError::Invalid)?
                .contract(),
            metadata,
            &self.task.detail.summary.investigation_id,
            ledger,
            &evidence,
            &saved_policy(self.task)?,
        )?;
        if self
            .task
            .detail
            .summary
            .specialist_id
            .uses_capacity_exclusions()
        {
            let refused = admission_journal::capacity_exclusion_candidates(
                self.connection,
                self.task,
                cutoff,
                ledger,
            )?;
            if self
                .task
                .detail
                .summary
                .specialist_id
                .uses_finish_feedback_witness()
            {
                return Ok(choices.with_capacity_obligations(
                    &self
                        .task
                        .completion_intent
                        .as_ref()
                        .ok_or(InvestigationStoreError::Invalid)?
                        .contract(),
                    metadata,
                    ledger,
                    &refused,
                    &saved_policy(self.task)?,
                )?);
            }
            return Ok(choices.excluding_capacity_refusals(
                &self
                    .task
                    .completion_intent
                    .as_ref()
                    .ok_or(InvestigationStoreError::Invalid)?
                    .contract(),
                metadata,
                ledger,
                &refused,
                &saved_policy(self.task)?,
            )?);
        }
        Ok(choices)
    }

    /// Ledger is the exact canonical row already associated with this ModelStarted
    /// event by the private timeline. This does not replace that event association.
    pub(in super::super) fn validate_timeline_call(
        &self,
        basis: &admission_contract::SavedCallBasis,
        ledger: &InvestigationInputLedger,
    ) -> Result<admission_contract::SavedCallCore, InvestigationStoreError> {
        self.check_epoch()?;
        let step = self.step(&basis.step_id)?;
        if ledger.coverage.as_ref() != self.archive.as_ref().map(|archive| archive.proof()) {
            return Err(InvestigationStoreError::Invalid);
        }
        let metadata = self.metadata_at(&basis.step_id)?;
        basis.validate_core(self.task, step, ledger, &metadata)?;
        self.acquisition_choices_at(basis, ledger)?;
        Ok(admission_contract::SavedCallCore {
            step: step.clone(),
            ledger: ledger.clone(),
            metadata,
            ordinal: basis.invocation_ordinal,
        })
    }
}
#[cfg(any(test, feature = "test-support"))]
thread_local! {static INVOCATION_SWEEPS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn note_invocation_sweep() {
    INVOCATION_SWEEPS.with(|count| count.set(count.get() + 1));
}
#[cfg(any(test, feature = "test-support"))]
#[path = "saved_metadata_history_tests.rs"]
mod tests;
