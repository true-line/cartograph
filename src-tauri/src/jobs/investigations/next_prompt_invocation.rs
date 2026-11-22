//! Owner-authenticated next-family pre-call boundaries. Public consent alone
//! never substitutes for the exact sealed step and execution owner.
use super::*;
use admission_contract::SavedCallBasis;
use llm::bounded::{AuthorizedBoundedCompletion, CompletionLimits, PreparedBoundedCompletion};

pub struct NextPromptInvocation<'a> {
    pub input: &'a InvestigationInput,
    pub prepared: &'a PreparedBoundedCompletion,
    /// Exact protected PRE-call RunClock sample, never a reset or lower usage.
    pub active_milliseconds: u64,
}

impl JobStore {
    pub fn await_next_prompt_consent(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &NextPromptInvocation<'_>,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.next_prompt_invocation_boundary(execution, revision, input, None, validate_retained)
    }

    pub fn begin_next_prompt_invocation(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &NextPromptInvocation<'_>,
        authorized: &AuthorizedBoundedCompletion,
        validate_retained: impl FnOnce(
            &crate::sources::RetainedSourceView<'_>,
        ) -> Result<(), InvestigationStoreError>,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.next_prompt_invocation_boundary(
            execution,
            revision,
            input,
            Some(authorized),
            validate_retained,
        )
    }

    fn next_prompt_invocation_boundary(
        &mut self,
        execution: &JobExecution,
        revision: u64,
        input: &NextPromptInvocation<'_>,
        authorized: Option<&AuthorizedBoundedCompletion>,
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
        let mut task = load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        transitions::check_job(&tx, execution, false)?;
        if task.detail.summary.revision != revision {
            return Err(InvestigationStoreError::Stale);
        }
        if task.detail.summary.cancel_requested {
            return Err(InvestigationStoreError::Cancelled);
        }
        let waiting = authorized.is_none();
        match (waiting, task.detail.provider.mode, task.phase) {
            (true, InvestigationProviderMode::Cloud, Phase::Ready)
            | (false, InvestigationProviderMode::Local, Phase::Ready)
            | (false, InvestigationProviderMode::Cloud, Phase::Consent) => {}
            _ => return Err(InvestigationStoreError::Stale),
        }
        let effective_policy = super::policy_for_detail(&task.detail)?;
        let policy = &effective_policy;
        let (step, basis) = prepare_saved_call(&tx, &mut task, input)?;
        if waiting {
            if task.pending.is_some()
                || task.approved
                || task
                    .admission_state
                    .as_ref()
                    .and_then(|s| s.current_call.as_ref())
                    .is_some()
            {
                return Err(InvestigationStoreError::Invalid);
            }
        } else {
            let authorized = authorized.ok_or(InvestigationStoreError::Invalid)?;
            let preview = input.prepared.preview();
            if authorized.profile() != &preview.profile
                || authorized.limits() != &preview.limits
                || authorized.action_id() != preview.egress.action_id
                || authorized.tier() != preview.egress.tier
                || authorized.payload_hash() != preview.egress.payload_hash
                || authorized.payload() != &preview.egress.payload
            {
                return Err(InvestigationStoreError::Invalid);
            }
            if task.detail.provider.mode == InvestigationProviderMode::Cloud {
                if !task.approved
                    || task.pending.as_ref() != Some(&step)
                    || task
                        .admission_state
                        .as_ref()
                        .and_then(|s| s.current_call.as_ref())
                        != Some(&basis)
                {
                    return Err(InvestigationStoreError::Stale);
                }
                let approved: InvestigationEvent = read_body(
                    &tx,
                    "investigation_events",
                    "sequence",
                    &id,
                    &signed(task.detail.summary.last_event_sequence)?,
                )?
                .ok_or(InvestigationStoreError::Missing)?;
                if approved.kind != InvestigationEventKind::ConsentApproved
                    || approved.step_id.as_deref() != Some(step.step_id.as_str())
                    || approved.investigation_id != id
                    || approved.sequence != task.detail.summary.last_event_sequence
                    || approved.revision != task.detail.summary.revision
                    || approved.tool.is_some()
                {
                    return Err(InvestigationStoreError::Invalid);
                }
            } else if task.pending.is_some()
                || task.approved
                || task
                    .admission_state
                    .as_ref()
                    .and_then(|s| s.current_call.as_ref())
                    .is_some()
            {
                return Err(InvestigationStoreError::Invalid);
            }
            completion::reserve_invocation(&tx, &task)?;
        }
        // Actual deadline/retained identities are host witnesses checked in this
        // same writer snapshot. This callback must acquire no new host locks.
        validate_retained(&crate::sources::RetainedSourceView::from_transaction(&tx))?;
        task.pending = Some(step.clone());
        task.approved = false;
        task.admission_state
            .as_mut()
            .ok_or(InvestigationStoreError::Invalid)?
            .current_call = Some(basis.clone());
        if waiting {
            task.phase = Phase::Consent;
            task.detail.summary.status = InvestigationStatus::AwaitingConsent;
            bump(&tx, &mut task)?;
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ConsentRequired,
                Some(&step.step_id),
                None,
                "Exact next-family step consent is required; no invocation was charged.",
            )?;
        } else {
            task.detail.usage.model_invocations = task
                .detail
                .usage
                .model_invocations
                .checked_add(1)
                .ok_or(InvestigationStoreError::Capacity)?;
            task.detail.usage.generated_token_reservations = task
                .detail
                .usage
                .generated_token_reservations
                .checked_add(step.generated_tokens)
                .ok_or(InvestigationStoreError::Capacity)?;
            validate_usage(&task.detail.usage, &policy.limits())?;
            insert_body(
                &tx,
                "investigation_invocations",
                &id,
                &step.step_id,
                &step,
                MAX_INVOCATION_BYTES,
            )?;
            task.phase = Phase::Invocation;
            task.detail.summary.status = InvestigationStatus::Running;
            bump(&tx, &mut task)?;
            event(
                &tx,
                &mut task,
                InvestigationEventKind::ModelStarted,
                Some(&step.step_id),
                None,
                "Next-family invocation boundary committed; the provider outcome is pending.",
            )?;
            basis.validate_saved(&tx, &task)?;
        }
        save_next_prompt_task(&tx, &task, revision, false)?;
        #[cfg(any(test, feature = "test-support"))]
        if FAIL_BEFORE_INVOCATION_COMMIT.with(|slot| slot.replace(false)) {
            return Err(InvestigationStoreError::Capacity);
        }
        tx.commit()?;
        #[cfg(any(test, feature = "test-support"))]
        if LOSE_INVOCATION_ACK.with(|slot| slot.replace(false)) {
            return Err(InvestigationStoreError::Storage);
        }
        Ok(task.detail)
    }
}

pub(super) fn prepare_saved_call(
    tx: &rusqlite::Transaction<'_>,
    task: &mut StoredTask,
    input: &NextPromptInvocation<'_>,
) -> Result<(PendingInvestigationStep, SavedCallBasis), InvestigationStoreError> {
    admission_contract::validate_pairing(task)?;
    let effective_policy = super::policy_for_detail(&task.detail)?;
    let policy = &effective_policy;
    let mut expected_limits = CompletionLimits::default().values();
    expected_limits.request_timeout_ms = task
        .completion_intent
        .as_ref()
        .ok_or(InvestigationStoreError::Invalid)?
        .effective_request_timeout_ms();
    expected_limits.max_output_tokens = input.input.invocation_output_tokens(policy)?;
    expected_limits.max_spans = u32::try_from(policy.limits().evidence_items)
        .map_err(|_| InvestigationStoreError::Invalid)?;
    if input.prepared.limits().values() != expected_limits {
        return Err(InvestigationStoreError::Invalid);
    }
    let provider = &task.detail.provider;
    let profile = input.prepared.profile();
    if profile.provider_id != provider.provider_id
        || profile.requested_model != provider.model
        || profile.endpoint_id != provider.endpoint
        || provider.protocol_version.as_deref() != Some(profile.protocol_version.as_str())
        || provider.deployment.is_some()
        || (profile.locality == llm::Locality::Local)
            != (provider.mode == InvestigationProviderMode::Local)
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
    let ledger = storage::load_ledger(tx, task)?.ok_or(InvestigationStoreError::Missing)?;
    if input.input.ledger() != &ledger {
        return Err(InvestigationStoreError::Conflict);
    }
    let ordinal = task
        .detail
        .usage
        .model_invocations
        .checked_add(1)
        .ok_or(InvestigationStoreError::Capacity)?;
    let step_id = format!("step-{ordinal}");
    let action_id = format!("{}:{step_id}", task.detail.summary.investigation_id);
    let action = input.input.action(action_id.clone())?;
    if action.payload.system != task.detail.summary.specialist_id.prompt()
        || !input.prepared.matches_action(&action)
        || input.prepared.preview().egress.action_id != action_id
    {
        return Err(InvestigationStoreError::Invalid);
    }
    let disclosure = input
        .input
        .seal_disclosure_with_policy(input.prepared, policy)?
        .ok_or(InvestigationStoreError::Invalid)?;
    let mut usage = task.detail.usage.clone();
    usage.active_milliseconds = input.active_milliseconds;
    if disclosure
        .admission
        .as_ref()
        .is_none_or(|stamp| stamp.consumed_budgets != usage)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    input.input.measure_v9_reserve_with_policy(
        profile,
        input.prepared.limits(),
        &action_id,
        policy,
    )?;
    let step = PendingInvestigationStep {
        step_id,
        payload_hash: input.prepared.preview().egress.payload_hash.clone(),
        input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
        input_bytes: encode(&action.payload, MAX_INPUT_BYTES)?.len(),
        generated_tokens: input.prepared.limits().values().max_output_tokens,
        disclosure: Some(disclosure),
    };
    validate_step(&step, &task.detail.limits)?;
    task.detail.usage = usage;
    let metadata = completion::metadata_current_next_prompt(tx, task)?;
    let basis = SavedCallBasis::from_pre_call_saved(tx, task, &step, &ledger, &metadata)?;
    Ok((step, basis))
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_BEFORE_INVOCATION_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LOSE_INVOCATION_ACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
#[path = "next_prompt_invocation_tests.rs"]
pub(super) mod tests;
