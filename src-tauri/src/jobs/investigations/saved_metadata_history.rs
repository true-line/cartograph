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
        let steps = next_prompt_invocations(connection, task)?;
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
        let cutoff = self.ordinal(step)?;
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
        Ok(admission_contract::SavedCallCore {
            step: step.clone(),
            ledger: ledger.clone(),
            metadata,
            ordinal: basis.invocation_ordinal,
        })
    }
}
#[cfg(test)]
thread_local! {static INVOCATION_SWEEPS:std::cell::Cell<usize>=const {std::cell::Cell::new(0)};}
#[cfg(test)]
pub(super) fn note_invocation_sweep() {
    INVOCATION_SWEEPS.with(|count| count.set(count.get() + 1));
}
#[cfg(test)]
#[path = "saved_metadata_history_tests.rs"]
mod tests;
