//! Task3/intent4 saved-call contract shared by public starts and owned workers.
//! Validation proves consistency against supplied/saved host witnesses, not a
//! dispatch permit, source-read authority or resistance to consistent local DB forgery.
use super::*;
use agents::investigation::staged_refusal::CompletionCallMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredAdmissionState {
    pub schema_version: u32,
    pub current_call: Option<SavedCallBasis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedCallBasis {
    pub schema_version: u32,
    pub task_id: String,
    pub request_hash: String,
    pub step_id: String,
    /// Immutable within-task ordinal, never a SQLite rowid or timestamp.
    pub invocation_ordinal: u32,
    pub input_ledger_hash: String,
    pub input_revision: u64,
    pub completion_metadata_hash: String,
    /// Exact sealed public stamp, including PRE-call usage and derived mode.
    pub admission: DisclosedAdmissionState,
}

pub(super) fn validate_pairing(task: &StoredTask) -> Result<(), InvestigationStoreError> {
    let intent = task
        .completion_intent
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?;
    if task.schema_version != 3
        || task.detail.summary.schema_version != 7
        || !task
            .detail
            .summary
            .specialist_id
            .uses_next_prompt_admission()
        || super::policy_for_detail(&task.detail)?.profile() != INVESTIGATION_V2_POLICY.profile()
        || intent.schema_version != 4
        || intent.request_schema_version != Some(7)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    Ok(())
}
impl StoredAdmissionState {
    pub(super) fn validate_task_binding(
        &self,
        task: &StoredTask,
    ) -> Result<(), InvestigationStoreError> {
        validate_pairing(task)?;
        if self.schema_version != 1 {
            return Err(InvestigationStoreError::Invalid);
        }
        let active = matches!(
            task.phase,
            Phase::Consent | Phase::Invocation | Phase::Action | Phase::Tool | Phase::Reservation
        );
        if active && self.current_call.is_none()
            || matches!(task.phase, Phase::Queued | Phase::Preparing | Phase::Ready)
                && self.current_call.is_some()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        if let Some(call) = &self.current_call {
            call.validate_saved_shape(
                task.detail.summary.specialist_id,
                &super::policy_for_detail(&task.detail)?,
            )?;
            if call.task_id != task.detail.summary.investigation_id
                || call.request_hash != task.intent_hash
                || task.ledger_hash.as_ref() != Some(&call.input_ledger_hash)
                || task.ledger_revision != Some(call.input_revision)
                || task.pending.as_ref().is_some_and(|step| {
                    step.step_id != call.step_id
                        || step.input_ledger_hash != call.input_ledger_hash
                        || step.disclosure.as_ref().and_then(|d| d.admission.as_ref())
                            != Some(&call.admission)
                })
            {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        Ok(())
    }
}
impl SavedCallBasis {
    pub(super) fn mode(&self) -> CompletionCallMode {
        self.admission.mode
    }
    fn validate_saved_shape(
        &self,
        saved_role: SpecialistId,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationStoreError> {
        if self.schema_version != 1
            || !id_valid(&self.task_id)
            || !id_valid(&self.step_id)
            || !hash_valid(&self.request_hash)
            || !hash_valid(&self.input_ledger_hash)
            || !hash_valid(&self.completion_metadata_hash)
            || i64::try_from(self.input_revision).is_err()
            || self.invocation_ordinal
                != self
                    .admission
                    .consumed_budgets
                    .model_invocations
                    .checked_add(1)
                    .ok_or(InvestigationStoreError::Capacity)?
            || self.invocation_ordinal > policy.limits().model_invocations
        {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_usage(&self.admission.consumed_budgets, &policy.limits())?;
        self.admission
            .validate_saved_role_shape(saved_role, policy)?;
        encode(self, MAX_INVOCATION_BYTES)?;
        Ok(())
    }
    /// Pure consistency construction. Supplied metadata and capacity state are
    /// NOT provenance; callers must obtain both from authentic saved journals.
    /// Use from_pre_call_saved for transaction-bound validation.
    pub(super) fn from_pre_call(
        task: &StoredTask,
        step: &PendingInvestigationStep,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
        capacity: &admission_journal::CapacityState,
    ) -> Result<Self, InvestigationStoreError> {
        validate_pairing(task)?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let admission = step
            .disclosure
            .as_ref()
            .and_then(|d| d.admission.clone())
            .ok_or(InvestigationStoreError::Invalid)?;
        let value = Self {
            schema_version: 1,
            task_id: task.detail.summary.investigation_id.clone(),
            request_hash: task.intent_hash.clone(),
            step_id: step.step_id.clone(),
            invocation_ordinal: admission
                .consumed_budgets
                .model_invocations
                .checked_add(1)
                .ok_or(InvestigationStoreError::Capacity)?,
            input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
            input_revision: ledger.revision,
            completion_metadata_hash: metadata.fingerprint_with_policy(policy)?,
            admission,
        };
        value.validate_core(task, step, ledger, metadata)?;
        if value.admission.consumed_budgets != task.detail.usage
            || task.ledger_hash.as_ref() != Some(&value.input_ledger_hash)
            || task.ledger_revision != Some(value.input_revision)
        {
            return Err(InvestigationStoreError::Stale);
        }
        value.validate_capacity_state(capacity)?;
        Ok(value)
    }
    /// Compare actual current journals and retained ledger in one caller Tx.
    /// This does not authenticate task/lease ownership, certify fit or dispatch.
    pub(super) fn from_pre_call_saved(
        connection: &Connection,
        task: &StoredTask,
        step: &PendingInvestigationStep,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
    ) -> Result<Self, InvestigationStoreError> {
        if connection.is_autocommit() {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_pairing(task)?;
        let id = &task.detail.summary.investigation_id;
        let prior: Option<PendingInvestigationStep> = read_body(
            connection,
            "investigation_invocations",
            "step_id",
            id,
            &step.step_id,
        )?;
        if prior.is_some() {
            return Err(InvestigationStoreError::Conflict);
        }
        let saved: InvestigationInputLedger = read_body(
            connection,
            "investigation_inputs",
            "revision",
            id,
            &signed(ledger.revision)?,
        )?
        .ok_or(InvestigationStoreError::Missing)?;
        if saved != *ledger {
            return Err(InvestigationStoreError::Invalid);
        }
        drop(saved);
        let archive = coverage::load_for_task(connection, task)?;
        if ledger.coverage.as_ref() != archive.as_ref().map(|archive| archive.proof()) {
            return Err(InvestigationStoreError::Invalid);
        }
        drop(archive);
        let actual_metadata = completion::metadata_current_next_prompt(connection, task)?;
        if actual_metadata != *metadata {
            return Err(InvestigationStoreError::Invalid);
        }
        drop(actual_metadata);
        let capacity = admission_journal::derive_current(connection, task)?;
        let value = Self::from_pre_call(task, step, ledger, metadata, &capacity)?;
        if task.detail.summary.specialist_id == SpecialistId::DomainAnalystV18 {
            let choices = completion::SavedMetadataHistory::load(connection, task)?
                .current_choices(ledger, metadata)?;
            if choices
                .as_ref()
                .map(|choices| choices.stamp_hash())
                .transpose()?
                != value
                    .admission
                    .acquisition
                    .as_ref()
                    .map(|stamp| stamp.choices_hash.clone())
            {
                return Err(InvestigationStoreError::Invalid);
            }
        }
        Ok(value)
    }
    /// Journal-independent core for replaying a historical saved call. Current
    /// task usage/ledger may have advanced; these exact witnesses may not.
    pub(super) fn validate_core(
        &self,
        task: &StoredTask,
        step: &PendingInvestigationStep,
        ledger: &InvestigationInputLedger,
        metadata: &ProtectedCompletionMetadata,
    ) -> Result<(), InvestigationStoreError> {
        validate_pairing(task)?;
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        ledger.validate_for_specialist(task.detail.summary.specialist_id, policy)?;
        let role = task.detail.summary.specialist_id;
        self.validate_saved_shape(role, policy)?;
        let intent = task
            .completion_intent
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        intent.validate(task)?;
        validate_step(step, &task.detail.limits)?;
        let disclosure = step
            .disclosure
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        disclosure.validate_saved_role_shape(ledger, policy, role)?;
        self.admission
            .validate_mode_with_metadata(metadata, policy)?;
        if self.task_id != task.detail.summary.investigation_id
            || self.request_hash != task.intent_hash
            || self.step_id != step.step_id
            || self.input_revision != ledger.revision
            || self.input_ledger_hash != step.input_ledger_hash
            || self.input_ledger_hash != ledger.fingerprint_with_policy(policy)?
            || self.completion_metadata_hash != metadata.fingerprint_with_policy(policy)?
            || metadata.request_hash != self.request_hash
            || metadata.obligations_hash != intent.obligations_hash
            || disclosure.schema_version != 6
            || disclosure.admission.as_ref() != Some(&self.admission)
            || disclosure.completion_metadata_hash.as_ref() != Some(&self.completion_metadata_hash)
            || disclosure.completion_contract_hash.as_deref()
                != Some(intent.contract().fingerprint_with_policy(policy)?.as_str())
            || disclosure
                .query_history
                .as_ref()
                .map(|h| h.request_hash.as_str())
                != Some(self.request_hash.as_str())
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    fn validate_capacity_state(
        &self,
        capacity: &admission_journal::CapacityState,
    ) -> Result<(), InvestigationStoreError> {
        if self
            .admission
            .acquisition
            .as_ref()
            .and_then(|stamp| stamp.finish_reason)
            != capacity
                .action_feedback
                .as_ref()
                .filter(|feedback| {
                    feedback.reason == agents::investigation::ActionRefusalReason::RepeatedRead
                })
                .map(|feedback| feedback.reason)
            || self.admission.capacity_refusal_count != capacity.count
            || self.admission.capacity_feedback != capacity.feedback
            || self
                .admission
                .acquisition
                .as_ref()
                .and_then(|stamp| stamp.action_feedback_hash.as_deref())
                != capacity
                    .action_feedback
                    .as_ref()
                    .map(|feedback| feedback.stamp_hash())
                    .transpose()?
                    .as_deref()
        {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    /// Validate current invocation debit without rederiving mode from charged
    /// usage. Other real tool/validation/time charges may have advanced.
    pub(super) fn validate_charged_usage(
        &self,
        task: &StoredTask,
        step: &PendingInvestigationStep,
    ) -> Result<(), InvestigationStoreError> {
        let pre = &self.admission.consumed_budgets;
        let current = &task.detail.usage;
        if current.model_invocations != self.invocation_ordinal
            || current.generated_token_reservations
                != pre
                    .generated_token_reservations
                    .checked_add(step.generated_tokens)
                    .ok_or(InvestigationStoreError::Capacity)?
            || current.tool_actions < pre.tool_actions
            || current.selected_facts < pre.selected_facts
            || current.evidence_requests < pre.evidence_requests
            || current.evidence_items < pre.evidence_items
            || current.evidence_bytes < pre.evidence_bytes
            || current.captured_validation_bytes < pre.captured_validation_bytes
            || current.active_milliseconds < pre.active_milliseconds
        {
            return Err(InvestigationStoreError::Invalid);
        }
        validate_usage(current, &task.detail.limits)
    }
    /// Consent retains an exact PRE-call witness but has neither an immutable
    /// invocation nor a model/token debit. Replay it against current journals,
    /// rather than treating approval as an already-started call.
    pub(super) fn validate_pending_consent(
        &self,
        connection: &Connection,
        task: &StoredTask,
    ) -> Result<(), InvestigationStoreError> {
        let state = task
            .admission_state
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        state.validate_task_binding(task)?;
        if task.phase != Phase::Consent
            || task.detail.provider.mode != InvestigationProviderMode::Cloud
            || state.current_call.as_ref() != Some(self)
        {
            return Err(InvestigationStoreError::Invalid);
        }
        let step = task
            .pending
            .as_ref()
            .ok_or(InvestigationStoreError::Missing)?;
        let ledger =
            storage::load_ledger(connection, task)?.ok_or(InvestigationStoreError::Missing)?;
        let metadata = completion::metadata_current_next_prompt(connection, task)?;
        if Self::from_pre_call_saved(connection, task, step, &ledger, &metadata)? != *self {
            return Err(InvestigationStoreError::Invalid);
        }
        Ok(())
    }
    /// Full current-call check against the authentic capacity journal strictly
    /// before this exact saved invocation. Journal replay uses only the core
    /// loader, so this comparison cannot recurse through validate_saved.
    pub(super) fn validate_saved(
        &self,
        connection: &Connection,
        task: &StoredTask,
    ) -> Result<(), InvestigationStoreError> {
        let state = task
            .admission_state
            .as_ref()
            .ok_or(InvestigationStoreError::Invalid)?;
        state.validate_task_binding(task)?;
        if state.current_call.as_ref() != Some(self) {
            return Err(InvestigationStoreError::Invalid);
        }
        if task.detail.summary.specialist_id == SpecialistId::DomainAnalystV18 {
            let (
                SavedCallCore {
                    step,
                    ledger,
                    metadata,
                    ..
                },
                capacity,
            ) = load_saved_call_basis_core_and_capacity(connection, task, self)?;
            drop((ledger, metadata));
            self.validate_capacity_state(&capacity)?;
            return self.validate_charged_usage(task, &step);
        }
        let SavedCallCore {
            step,
            ledger,
            metadata,
            ..
        } = load_saved_call_basis_core(connection, task, self)?;
        drop((ledger, metadata));
        let capacity = admission_journal::derive_at(connection, task, &self.step_id)?;
        self.validate_capacity_state(&capacity)?;
        self.validate_charged_usage(task, &step)
    }
}

pub(super) struct SavedCallCore {
    pub step: PendingInvestigationStep,
    pub ledger: InvestigationInputLedger,
    pub metadata: ProtectedCompletionMetadata,
    /// Explicit immutable within-task ordinal. Never SQLite rowid.
    pub ordinal: u32,
}
/// Same-transaction @18 core and strict-cutoff capacity from one full replay.
/// No replay cache survives this call; standalone loaders remain available.
pub(super) fn load_saved_call_basis_core_and_capacity(
    connection: &Connection,
    task: &StoredTask,
    basis: &SavedCallBasis,
) -> Result<(SavedCallCore, admission_journal::CapacityState), InvestigationStoreError> {
    if connection.is_autocommit()
        || task.detail.summary.specialist_id != SpecialistId::DomainAnalystV18
    {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_pairing(task)?;
    admission_journal::saved_core_and_capacity(connection, task, basis)
}

/// Exact named ledger/step/cutoff; @18 authenticates the full journal prefix.
/// The caller owns the DB transaction and separately authenticates task/lease.
pub(super) fn load_saved_call_basis_core(
    connection: &Connection,
    task: &StoredTask,
    basis: &SavedCallBasis,
) -> Result<SavedCallCore, InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    validate_pairing(task)?;
    if task.detail.summary.specialist_id == SpecialistId::DomainAnalystV18 {
        return admission_journal::saved_core(connection, task, basis);
    }
    let id = &task.detail.summary.investigation_id;
    if basis.task_id != *id {
        return Err(InvestigationStoreError::Invalid);
    }
    let ledger: InvestigationInputLedger = read_body(
        connection,
        "investigation_inputs",
        "revision",
        id,
        &signed(basis.input_revision)?,
    )?
    .ok_or(InvestigationStoreError::Missing)?;
    let archive = coverage::load_for_task(connection, task)?;
    if ledger.coverage.as_ref() != archive.as_ref().map(|archive| archive.proof()) {
        return Err(InvestigationStoreError::Invalid);
    }
    drop(archive);
    completion::SavedMetadataHistory::load(connection, task)?.validate_timeline_call(basis, &ledger)
}
