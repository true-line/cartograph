//! Owner-authenticated source reservation. A successful commit reserves later
//! work; it grants neither source access nor provider dispatch.
use super::*;
use llm::bounded::PreparedBoundedCompletion;
use read_reservations::{ReadBinding, Reservation, StorageClaim};
use serde_json::Value;

/// Trusted native adapter over a held prepared receipt and its source guards.
/// Implementations must reject a different target or base ledger and resolve
/// sizes from that held receipt, never from caller-supplied size claims.
/// This borrow must remain live through reservation and settlement.
pub trait SourceSizeBinding {
    fn sizes_for(
        &self,
        target: &InvestigationInspectionTarget,
        ledger: &InvestigationInputLedger,
    ) -> Result<Option<core_redact::DirectSourceJsonContentSizes>, InvestigationError>;
}

pub struct SourceAdmission<'a> {
    pub size_binding: Option<&'a dyn SourceSizeBinding>,
    pub response: &'a InvestigationResponseMeta,
    pub original_action: &'a Value,
    pub current: &'a InvestigationInput,
    pub current_prepared: &'a PreparedBoundedCompletion,
    pub proof: &'a UnreadSourcePreviewBound,
    pub target: &'a InvestigationInspectionTarget,
    /// Later host RunClock sample checks the current deadline only. Persisted
    /// reservation base usage remains exactly the saved response's admitted
    /// usage; validation/settlement own their subsequent observed accounting.
    pub active_milliseconds: u64,
}

/// Only issued after COMMIT acknowledgement. This is not a read permit: the
/// independent owner-authenticated validation charge and access fence must run.
pub struct CommittedSourceReservation {
    task_id: String,
    step_id: String,
    reservation_hash: String,
    source_descriptor_hash: String,
    base_ledger_hash: String,
    detail: InvestigationDetail,
}
impl CommittedSourceReservation {
    /// Match an acknowledged reservation against the immutable saved record.
    /// Charge and cancellation may advance the task revision, never this identity.
    pub(super) fn matches_saved(
        &self,
        saved: &read_reservations::Reservation,
    ) -> Result<bool, InvestigationStoreError> {
        Ok(self.task_id == saved.binding.task_id
            && self.step_id == saved.binding.step_id
            && self.reservation_hash == saved.fingerprint()?
            && self.source_descriptor_hash == saved.source.target_id
            && self.base_ledger_hash == saved.binding.base_ledger_hash)
    }
    pub fn basis(&self) -> SourceValidationBasis<'_> {
        SourceValidationBasis {
            task_id: &self.task_id,
            step_id: &self.step_id,
            reservation_hash: &self.reservation_hash,
            revision: self.detail.summary.revision,
        }
    }
    pub fn matches(&self, target: &InvestigationInspectionTarget, ledger_hash: &str) -> bool {
        target.validate().is_ok()
            && self.source_descriptor_hash == target.target_id
            && self.base_ledger_hash == ledger_hash
    }
    pub fn detail(&self) -> &InvestigationDetail {
        &self.detail
    }
}

impl JobStore {
    pub fn admit_next_prompt_source(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &SourceAdmission<'_>,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<CommittedSourceReservation, InvestigationStoreError> {
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
        if task.detail.summary.revision != revision || task.phase != Phase::Invocation {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            transitions::check_job(&tx, execution, true)?;
            return Err(InvestigationStoreError::Cancelled);
        }
        transitions::check_job(&tx, execution, false)?;
        if task.pending_source_read.is_some() || task.action_tool.is_some() {
            return Err(InvestigationStoreError::Invalid);
        }
        let authenticated = metadata_admission::authenticate_response(
            &tx,
            &task,
            input.response,
            input.original_action,
            input.current,
            input.current_prepared,
            input.active_milliseconds,
        )?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let call = &authenticated.call;
        let core = &authenticated.core;
        if completion::metadata_current_next_prompt(&tx, &task)? != core.metadata {
            return Err(InvestigationStoreError::Invalid);
        }
        let context_id = &core
            .ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?
            .context_id;
        let read = match ProgressiveInvestigationAction::decode_resolved_for_specialist(
            std::str::from_utf8(&authenticated.original)
                .map_err(|_| InvestigationStoreError::Invalid)?,
            context_id,
            &call.request_hash,
            &core.ledger,
            policy,
            task.detail.summary.specialist_id,
        )? {
            CompletionInvestigationAction::Read(QualifiedInvestigationAction::SourceOrFinish(
                InvestigationAction::ReadEvidence { fact, role, index },
            )) => ReadIntent::Source { fact, role, index },
            _ => return Err(InvestigationStoreError::Invalid),
        };
        // The source append preserves the receipt set. Its zero-reference-row
        // allocation is sound only if every retained reference already exists.
        for reference in &core.ledger.receipt_references {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM investigation_refs WHERE task_id=?1 AND source_id=?2 AND repo_key=?3 AND receipt_id=?4 AND input_revision<=?5)",
                params![id, reference.source_id, reference.repo_key, reference.receipt_id, signed(core.ledger.revision)?], |row| row.get(0))?;
            if !exists {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        input.target.validate()?;
        if !matches!(&read, ReadIntent::Source {fact,role,index} if fact==&input.target.fact && role==&input.target.role && index==&input.target.index)
            || input.response.action_hash != staged::read_action_hash(&read)?
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let sizes = if task
            .detail
            .summary
            .specialist_id
            .uses_exact_source_reservation()
        {
            input
                .size_binding
                .as_ref()
                .map(|binding| binding.sizes_for(input.target, input.current.ledger()))
                .transpose()?
                .flatten()
        } else {
            None
        };
        if let Some(sizes) = sizes {
            input.proof.verify_measured_with_policy(
                input.current,
                input.current_prepared,
                input.target,
                input.current_prepared.profile(),
                input.current_prepared.limits(),
                &authenticated.next_action_id,
                policy,
                sizes,
            )?;
        } else {
            input.proof.verify_with_policy(
                input.current,
                input.current_prepared,
                input.target,
                input.current_prepared.profile(),
                input.current_prepared.limits(),
                &authenticated.next_action_id,
                policy,
            )?;
        }

        let span_bytes = input
            .target
            .source
            .byte_end
            .checked_sub(input.target.source.byte_start)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or(InvestigationStoreError::Invalid)?;
        if authenticated
            .admitted_usage
            .evidence_items
            .checked_add(1)
            .is_none_or(|items| items > task.detail.limits.evidence_items)
            || authenticated
                .admitted_usage
                .evidence_bytes
                .checked_add(span_bytes)
                .is_none_or(|bytes| bytes > task.detail.limits.evidence_bytes)
        {
            return Err(InvestigationStoreError::Capacity);
        }
        let execution_identity = task
            .execution
            .as_ref()
            .ok_or(InvestigationStoreError::Ownership)?;
        let mut reservation = Reservation {
            schema_version: 3,
            source: input.target.clone(),
            // Private structurally valid seed, not an admitted claim/estimate.
            // Only exact production projections below determine stored values.
            storage_claim: StorageClaim {
                validation_bytes: 1,
                settlement_bytes: 1,
                validation_events: 1,
                settlement_events: 2,
                settlement_attempts: 0,
            },
            binding: ReadBinding {
                task_id: id.clone(),
                step_id: call.step_id.clone(),
                request_hash: call.request_hash.clone(),
                specialist_id: task.detail.summary.specialist_id,
                policy_profile: policy.profile().into(),
                effective_limits: (policy.limits() != INVESTIGATION_V2_POLICY.limits())
                    .then(|| policy.limits()),
                execution_job: execution_identity.job_id,
                execution_generation: execution_identity.generation,
                owner_basis_hash: source_validation::owner_basis_hash(&task)?,
                original_response_hash: input.response.response_hash.clone(),
                original_action_hash: input.response.action_hash.clone(),
                resolved_read_hash: read.fingerprint()?,
                original_read: read,
                base_ledger_hash: call.input_ledger_hash.clone(),
                base_ledger_revision: call.input_revision,
                context_id: context_id.clone(),
                archive_hash: source_validation::retained_archive_hash(&tx, &id)?,
                base_usage: authenticated.admitted_usage.clone(),
                prior_completion_hash: core.metadata.fingerprint_with_policy(policy)?,
                certification_basis_hash: input.proof.basis_hash().into(),
            },
        };
        task.phase = Phase::Reservation;
        task.pending = None;
        task.approved = false;
        task.detail.usage = authenticated.admitted_usage;
        task.pending_source_read = Some(source_validation::PendingSourceRead {
            step_id: call.step_id.clone(),
            reservation_hash: reservation.fingerprint()?,
        });
        bump(&tx, &mut task)?;
        let mut projected = task.clone();
        let created_at = timestamp(&tx)?;
        let events = [
            prepare_event(
                &mut projected,
                InvestigationEventKind::ModelCompleted,
                Some(&call.step_id),
                None,
                "A complete source-read response was recorded.",
                created_at.clone(),
            )?,
            prepare_event(
                &mut projected,
                InvestigationEventKind::ActionAdmitted,
                Some(&call.step_id),
                Some(InvestigationTool::ReadEvidence),
                "The exact source read passed continuation admission; access is still uncharged.",
                created_at,
            )?,
        ];
        task = projected;
        super::semantic_audit_state::account_analyst_save(&mut task)?;
        let validation = source_validation::validation_storage_bound(&tx, &task, &reservation)?;
        let envelope = if task.detail.summary.specialist_id.uses_acquisition_choices() {
            source_settlement::envelope_with_authenticated_core(
                &tx,
                &task,
                &reservation,
                input.response,
                core,
            )?
        } else {
            source_settlement::envelope(&tx, &task, &reservation, input.response)?
        };
        reservation.storage_claim = StorageClaim {
            validation_bytes: validation,
            settlement_bytes: envelope.bytes,
            validation_events: 1,
            settlement_events: 2,
            settlement_attempts: envelope.attempts,
        };
        if reservation.storage_claim.bytes(false)? > TASK_CAPACITY
            || envelope.attempts > policy.max_completed_attempts()
        {
            return Err(InvestigationStoreError::Capacity);
        }
        task.pending_source_read
            .as_mut()
            .ok_or(InvestigationStoreError::Invalid)?
            .reservation_hash = reservation.fingerprint()?;
        let repeated = if task.detail.summary.specialist_id.uses_acquisition_choices() {
            source_settlement::envelope_with_authenticated_core(
                &tx,
                &task,
                &reservation,
                input.response,
                core,
            )?
        } else {
            source_settlement::envelope(&tx, &task, &reservation, input.response)?
        };
        if repeated != envelope
            || source_validation::validation_storage_bound(&tx, &task, &reservation)? != validation
        {
            return Err(InvestigationStoreError::Invalid);
        }
        count_headroom(
            &tx,
            &task,
            &reservation,
            &envelope,
            core.metadata.completed_attempt_ids.len(),
        )?;
        // Host retains the exact descriptor's existing source/registry guards
        // until acknowledgement; no new locks may be acquired in this callback.
        validate_retained(&crate::sources::RetainedSourceView::from_transaction(&tx))?;
        insert_body(
            &tx,
            "investigation_responses",
            &id,
            &call.step_id,
            input.response,
            4096,
        )?;
        let mut reads = read_reservations::ReadTransaction::new(&tx)?;
        reads.initialize()?;
        reads.insert_reservation(&reservation)?;
        for event in events {
            persist_event(&tx, &event)?;
        }
        source_validation::validate_saved_pending(&tx, &task)?;
        save_next_prompt_task(&tx, &task, revision, false)?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_BEFORE_SOURCE_ADMISSION_COMMIT.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        let capability = CommittedSourceReservation {
            task_id: id,
            step_id: reservation.binding.step_id.clone(),
            reservation_hash: reservation.fingerprint()?,
            source_descriptor_hash: reservation.source.target_id,
            base_ledger_hash: reservation.binding.base_ledger_hash,
            detail: task.detail,
        };
        drop(reads);
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
        if LOSE_SOURCE_ADMISSION_ACK.with(|flag| flag.replace(false)) {
            return Err(InvestigationStoreError::Storage);
        }
        Ok(capability)
    }
}

fn count_headroom(
    tx: &rusqlite::Transaction<'_>,
    task: &StoredTask,
    reservation: &Reservation,
    envelope: &source_settlement::SettlementEnvelope,
    attempt_count: usize,
) -> Result<(), InvestigationStoreError> {
    let effective_policy = super::policy_for_detail(&task.detail)?;
    let policy = &effective_policy;
    let id = &task.detail.summary.investigation_id;
    if envelope.events > reservation.storage_claim.settlement_events
        || envelope.input_rows > 1
        || envelope.reference_rows != 0
        || attempt_count
            .checked_add(envelope.attempts)
            .is_none_or(|count| count > policy.max_completed_attempts())
        || task
            .detail
            .summary
            .last_event_sequence
            .checked_add(reservation.storage_claim.events(false) as u64)
            .and_then(|count| count.checked_add(3))
            .is_none_or(|count| count > policy.journal_events())
    {
        return Err(InvestigationStoreError::Capacity);
    }
    for (table, added, ceiling) in [
        (
            "investigation_inputs",
            envelope.input_rows,
            task.detail.limits.tool_actions as usize + 1,
        ),
        (
            "investigation_refs",
            envelope.reference_rows,
            task.detail.limits.selected_facts,
        ),
        (
            "investigation_responses",
            1,
            task.detail.limits.model_invocations as usize,
        ),
    ] {
        let count: i64 = tx.query_row(
            &format!("SELECT count(*) FROM {table} WHERE task_id=?1"),
            [id],
            |row| row.get(0),
        )?;
        if usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_add(added))
            .is_none_or(|count| count > ceiling)
        {
            return Err(InvestigationStoreError::Capacity);
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_BEFORE_SOURCE_ADMISSION_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_SOURCE_ADMISSION_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn lose_next_commit_ack() {
    LOSE_SOURCE_ADMISSION_ACK.with(|flag| flag.set(true));
}

#[cfg(any(test, feature = "test-support"))]
#[path = "source_admission_tests.rs"]
mod tests;
