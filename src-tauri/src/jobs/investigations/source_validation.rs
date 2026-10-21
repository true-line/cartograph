//! Authenticated @10 source validation charge. This consumes an already admitted
//! saved reservation; it neither admits one nor certifies continuation capacity.
use super::*;
use read_reservations::{SavedRead, ValidationCharge};

/// Expected saved identities are CAS inputs, not caller-supplied authority;
/// every value is compared to the owner-resolved task and immutable journal.
pub(crate) struct SourceValidationBasis<'a> {
    pub task_id: &'a str,
    pub step_id: &'a str,
    pub reservation_hash: &'a str,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingSourceRead {
    pub step_id: String,
    pub reservation_hash: String,
}
impl PendingSourceRead {
    pub(super) fn validate(&self) -> Result<(), InvestigationStoreError> {
        if !id_valid(&self.step_id) || !hash_valid(&self.reservation_hash) {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
}

/// Issued only after the owner-authenticated writer COMMIT succeeds. No Clone,
/// Deserialize, public fields or constructor: a supplied DTO is not a receipt.
pub(crate) struct CommittedSourceValidation {
    task_id: String,
    step_id: String,
    reservation_hash: String,
    source_descriptor_hash: String,
    base_ledger_hash: String,
    validation_bytes: u64,
    detail: InvestigationDetail,
}
impl CommittedSourceValidation {
    pub(crate) fn matches(
        &self,
        basis: &SourceValidationBasis<'_>,
        target: &InvestigationInspectionTarget,
        base_ledger_hash: &str,
        bytes: u64,
    ) -> bool {
        target.validate().is_ok()
            && self.source_descriptor_hash == target.target_id
            && self.base_ledger_hash == base_ledger_hash
            && self.validation_bytes == bytes
            && bytes == target.captured.file.byte_len
            && self.task_id == basis.task_id
            && self.step_id == basis.step_id
            && self.reservation_hash == basis.reservation_hash
    }
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }
    pub(crate) fn step_id(&self) -> &str {
        &self.step_id
    }
    pub(crate) fn reservation_hash(&self) -> &str {
        &self.reservation_hash
    }
    pub(crate) fn detail(&self) -> &InvestigationDetail {
        &self.detail
    }
}

/// Private deterministic owner binding. Never persist/log the token itself in
/// a reservation. Identity still requires the live OS handle and job checks.
pub(super) fn owner_basis_hash(task: &StoredTask) -> Result<String, InvestigationStoreError> {
    let identity = task
        .execution
        .as_ref()
        .ok_or(InvestigationStoreError::Ownership)?;
    let mut bytes = b"cartograph:source-read-owner:v1\0".to_vec();
    bytes.extend_from_slice(&encode(identity, 4096)?);
    Ok(core_prov::content_hash(&bytes))
}

/// Classify absent/corrupt retained identity separately from SQLite I/O errors.
fn retained_archive_hash(
    connection: &Connection,
    task_id: &str,
) -> Result<String, InvestigationStoreError> {
    let row: Option<Option<String>> = connection.query_row(
        "SELECT CASE WHEN typeof(body_hash)='text' AND length(CAST(body_hash AS BLOB))=64 THEN body_hash END FROM investigation_coverage_archives WHERE task_id=?1",
        [task_id], |row| row.get(0),
    ).optional()?;
    let hash = row
        .ok_or(InvestigationStoreError::Missing)?
        .filter(|hash| hash_valid(hash))
        .ok_or(InvestigationStoreError::Invalid)?;
    Ok(hash)
}

/// Recheck all saved authority for the pending source slot in the caller's Tx.
/// Guards/source bytes and continuation certification remain host authorities.
pub(super) fn validate_saved_pending(
    connection: &Connection,
    task: &StoredTask,
) -> Result<SavedRead, InvestigationStoreError> {
    if connection.is_autocommit() || task.phase != Phase::Reservation {
        return Err(InvestigationStoreError::Invalid);
    }
    let pending = task
        .pending_source_read
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    pending.validate()?;
    let saved = read_reservations::load(
        connection,
        &task.detail.summary.investigation_id,
        &pending.step_id,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    let r = &saved.reservation;
    let b = &r.binding;
    if saved.disposition.is_some() || r.fingerprint() != Ok(pending.reservation_hash.clone()) {
        return Err(InvestigationStoreError::Conflict);
    }
    let call = task
        .admission_state
        .as_ref()
        .and_then(|state| state.current_call.as_ref())
        .ok_or(InvestigationStoreError::Invalid)?;
    call.validate_saved(connection, task)?;
    if call.mode() != CompletionCallMode::Choice {
        return Err(InvestigationStoreError::Invalid);
    }
    let core = admission_contract::load_saved_call_basis_core(connection, task, call)?;
    let execution = task
        .execution
        .as_ref()
        .ok_or(InvestigationStoreError::Ownership)?;
    let proof = core
        .ledger
        .coverage
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    let archive_hash = retained_archive_hash(connection, &b.task_id)?;
    let response: InvestigationResponseMeta = read_body(
        connection,
        "investigation_responses",
        "step_id",
        &b.task_id,
        &b.step_id,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    let selected = core
        .ledger
        .selected_facts
        .iter()
        .find(|s| s.fact == r.source.fact)
        .ok_or(InvestigationStoreError::Invalid)?;
    let binding = selected
        .binding
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if b.task_id != task.detail.summary.investigation_id
        || b.step_id != call.step_id
        || b.request_hash != task.intent_hash
        || b.specialist_id != task.detail.summary.specialist_id
        || b.policy_profile != task.detail.limits.profile
        || b.execution_job != execution.job_id
        || b.execution_generation != execution.generation
        || b.owner_basis_hash != owner_basis_hash(task)?
        || b.base_ledger_hash != call.input_ledger_hash
        || b.base_ledger_revision != call.input_revision
        || b.context_id != proof.context_id
        || b.archive_hash != archive_hash
        || b.prior_completion_hash
            != core
                .metadata
                .fingerprint_with_policy(&INVESTIGATION_V2_POLICY)?
        || response.step_id != b.step_id
        || response.response_hash != b.original_response_hash
        || response.action_hash != b.original_action_hash
        || !hash_valid(&response.response_hash)
        || !hash_valid(&response.action_hash)
        || r.source.graph_snapshot_id != core.ledger.graph_snapshot_id
        || r.source.fact_digest != selected.fact_digest
        || binding.receipt_id != r.source.receipt_id
        || binding.repo_key != r.source.source.repo
        || binding.emitted_fact_digest != r.source.fact_digest
        || !core.ledger.receipt_references.iter().any(|reference| {
            reference.source_id == r.source.registered_source_id
                && reference.repo_key == binding.repo_key
                && reference.receipt_id == binding.receipt_id
        })
    {
        return Err(InvestigationStoreError::Invalid);
    }
    // Reconstruct admission usage from the actual saved pre-call stamp and one
    // actual response, without adding reported tokens a second time on reopen.
    let mut base = call.admission.consumed_budgets.clone();
    if response.active_milliseconds < base.active_milliseconds {
        return Err(InvestigationStoreError::Invalid);
    }
    if let Some(model) = &response.observed_model
        && (model.is_empty() || model.len() > 256 || redacted_history_text(model, 256)? != *model)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    base.model_invocations = call.invocation_ordinal;
    base.generated_token_reservations = base
        .generated_token_reservations
        .checked_add(core.step.generated_tokens)
        .ok_or(InvestigationStoreError::Capacity)?;
    base.reported_input_tokens = transitions::add_reported(
        base.reported_input_tokens,
        response.reported_input_tokens,
        base.model_invocations == 1,
    )?;
    base.reported_output_tokens = transitions::add_reported(
        base.reported_output_tokens,
        response.reported_output_tokens,
        base.model_invocations == 1,
    )?;
    base.active_milliseconds = response.active_milliseconds;
    if base != b.base_usage {
        return Err(InvestigationStoreError::Invalid);
    }
    let mut current = base;
    if let Some(charge) = &saved.charge {
        charge.validate(r)?;
        current.captured_validation_bytes = current
            .captured_validation_bytes
            .checked_add(charge.validation_bytes)
            .ok_or(InvestigationStoreError::Capacity)?;
    }
    if current != task.detail.usage {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_usage(&current, &task.detail.limits)?;
    Ok(saved)
}

impl JobStore {
    /// Last owner/cancellation fence after charge COMMIT and before source IO.
    /// This read-only snapshot performs no retry or settlement and releases its
    /// DB borrow before returning; the host keeps retained source guards alive.
    pub(crate) fn verify_source_validation_access(
        &self,
        execution: &JobExecution,
        receipt: &CommittedSourceValidation,
    ) -> Result<(), InvestigationStoreError> {
        execution.verify()?;
        if execution.inner.namespace != self.namespace {
            return Err(InvestigationStoreError::Ownership);
        }
        let tx = self.conn.unchecked_transaction()?;
        validate(&tx, &self.namespace)?;
        let task = load_next_prompt_task_base(&tx, &receipt.task_id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if task.detail.summary.revision != receipt.detail.summary.revision {
            return Err(InvestigationStoreError::Stale);
        }
        let saved = validate_saved_pending(&tx, &task)?;
        let charge = saved.charge.ok_or(InvestigationStoreError::Missing)?;
        if saved.reservation.binding.step_id != receipt.step_id
            || charge.reservation_hash != receipt.reservation_hash
            || charge.source_descriptor_hash != receipt.source_descriptor_hash
            || charge.validation_bytes != receipt.validation_bytes
            || saved.reservation.binding.base_ledger_hash != receipt.base_ledger_hash
        {
            return Err(InvestigationStoreError::Conflict);
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn charge_prepared_source_validation(
        &mut self,
        execution: &JobExecution,
        basis: &SourceValidationBasis<'_>,
        target: &InvestigationInspectionTarget,
        base_ledger_hash: &str,
    ) -> Result<CommittedSourceValidation, InvestigationStoreError> {
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
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        if task.detail.summary.revision != basis.revision {
            return Err(InvestigationStoreError::Stale);
        }
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        if task.phase != Phase::Reservation {
            return Err(InvestigationStoreError::Stale);
        }
        let saved = validate_saved_pending(&tx, &task)?;
        let reservation = saved.reservation;
        if saved.charge.is_some() {
            return Err(InvestigationStoreError::Conflict);
        }
        if id != basis.task_id
            || reservation.binding.step_id != basis.step_id
            || reservation.fingerprint()?.as_str() != basis.reservation_hash
            || reservation.source != *target
            || reservation.binding.base_ledger_hash != base_ledger_hash
        {
            return Err(InvestigationStoreError::Conflict);
        }
        transitions::available_time(&task)?;
        let charge = ValidationCharge::for_reservation(&reservation)?;
        task.detail.usage.captured_validation_bytes = task
            .detail
            .usage
            .captured_validation_bytes
            .checked_add(charge.validation_bytes)
            .ok_or(InvestigationStoreError::Capacity)?;
        validate_usage(&task.detail.usage, &task.detail.limits)?;
        let mut reads = read_reservations::ReadTransaction::new(&tx)?;
        reads.insert_charge(&id, &reservation.binding.step_id, &charge)?;
        #[cfg(test)]
        inject_failure(ChargeFailure::AfterWitness)?;
        bump(&tx, &mut task)?;
        event(
            &tx,
            &mut task,
            InvestigationEventKind::EvidenceValidationReserved,
            Some(&reservation.binding.step_id),
            Some(InvestigationTool::ReadEvidence),
            "Exact captured-file validation charge committed before source access.",
        )?;
        save_next_prompt_task(&tx, &task, basis.revision, false)?;
        #[cfg(test)]
        inject_failure(ChargeFailure::AfterTaskSave)?;
        validate_saved_pending(&tx, &task)?;
        #[cfg(test)]
        inject_failure(ChargeFailure::BeforeCommit)?;
        tx.commit()?;
        #[cfg(test)]
        inject_failure(ChargeFailure::AfterCommit)?;
        // No receipt escapes on failed/uncertain commit, hence no source read.
        Ok(CommittedSourceValidation {
            task_id: id,
            step_id: reservation.binding.step_id,
            reservation_hash: charge.reservation_hash,
            source_descriptor_hash: charge.source_descriptor_hash,
            base_ledger_hash: reservation.binding.base_ledger_hash,
            validation_bytes: charge.validation_bytes,
            detail: task.detail,
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChargeFailure {
    AfterWitness,
    AfterTaskSave,
    BeforeCommit,
    AfterCommit,
}
#[cfg(test)]
thread_local! {static CHARGE_FAILURE:std::cell::Cell<Option<ChargeFailure>>=const {std::cell::Cell::new(None)};}
#[cfg(test)]
fn inject_failure(at: ChargeFailure) -> Result<(), InvestigationStoreError> {
    CHARGE_FAILURE.with(|slot| {
        if slot.get() == Some(at) {
            slot.set(None);
            Err(InvestigationStoreError::Storage)
        } else {
            Ok(())
        }
    })
}

#[cfg(test)]
#[path = "source_validation_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::fixture_data::{SourceValidationInitialData, captured_source_fixture};
