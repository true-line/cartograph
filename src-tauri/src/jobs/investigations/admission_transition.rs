//! Atomic capacity-refusal effects inside the caller's authenticated writer Tx.
//! The caller verifies execution ownership/cancellation and commits or rolls
//! back. This internal body grants no dispatch authority or public @10 route.
use super::*;
use agents::investigation::staged_refusal::CompletionCallMode;
use rusqlite::Transaction;
use serde_json::Value;

// Refusal-local, thread-local and consumed once. Production has no failure hook.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_AFTER_REFUSAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl JobStore {
    /// Host-only refusal publication. The worker supplies the reason from its
    /// continuation proof; this method authenticates durable write authority.
    /// No public start route or provider/tool execution is introduced here.
    pub fn refuse_next_prompt_capacity(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        response: &InvestigationResponseMeta,
        proposal: &Value,
        reason: CapacityRefusalReason,
    ) -> Result<InvestigationDetail, InvestigationStoreError> {
        self.refuse_next_prompt(
            execution,
            expected_revision,
            response,
            proposal,
            admission_journal::RefusalReason::Capacity(reason),
        )
    }
    pub fn refuse_next_prompt(
        &mut self,
        execution: &JobExecution,
        expected_revision: u64,
        response: &InvestigationResponseMeta,
        proposal: &Value,
        reason: admission_journal::RefusalReason,
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
        let task = storage::load_next_prompt_task_base(&tx, &id)?;
        transitions::check_identity(&task, execution)?;
        if task.detail.summary.revision != expected_revision {
            return Err(InvestigationStoreError::Stale);
        }
        transitions::check_job(&tx, execution, false)?;
        let next = match reason {
            admission_journal::RefusalReason::Capacity(reason) => {
                refuse_capacity(&tx, &task, response, proposal, reason)?
            }
            reason => refuse(&tx, &task, response, proposal, reason)?,
        };
        tx.commit()?;
        // No in-memory acknowledgement is returned before durable commit.
        Ok(next.detail)
    }
}

pub(super) fn refuse_capacity(
    tx: &Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    proposal: &Value,
    reason: CapacityRefusalReason,
) -> Result<StoredTask, InvestigationStoreError> {
    refuse(
        tx,
        task,
        response,
        proposal,
        admission_journal::RefusalReason::Capacity(reason),
    )
}
fn refuse(
    tx: &Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    proposal: &Value,
    reason: admission_journal::RefusalReason,
) -> Result<StoredTask, InvestigationStoreError> {
    storage::validate_next_prompt_task(task)?;
    if task.phase != Phase::Invocation {
        return Err(InvestigationStoreError::Stale);
    }
    if task.detail.summary.cancel_requested {
        return Err(InvestigationStoreError::Cancelled);
    }
    // Do not publish effects from a stale or substituted in-memory task. Lease
    // authentication is a separate caller responsibility in this same Tx.
    let id = &task.detail.summary.investigation_id;
    let expected = encode(task, MAX_RECORD_BYTES)?;
    let saved = storage::load_next_prompt_task_base(tx, id)?;
    if encode(&saved, MAX_RECORD_BYTES)? != expected {
        return Err(InvestigationStoreError::Stale);
    }
    let call = task
        .admission_state
        .as_ref()
        .and_then(|state| state.current_call.as_ref())
        .ok_or(InvestigationStoreError::Invalid)?;
    if call.mode() != CompletionCallMode::Choice {
        // Worker settles the distinct terminal protocol outcome. A refusal here
        // must never create feedback or permit another model call.
        return Err(InvestigationStoreError::Invalid);
    }
    let (_, mut usage) = transitions::validate_response(tx, task, response)?;
    if matches!(
        reason,
        admission_journal::RefusalReason::Action(
            agents::investigation::ActionRefusalReason::RepeatedRead
        )
    ) {
        // The saved journal proves this is an already acquired read. Charge
        // this proposal once without claiming another tool execution/result.
        usage.tool_actions = usage
            .tool_actions
            .checked_add(1)
            .ok_or(InvestigationStoreError::Capacity)?;
        validate_usage(&usage, &task.detail.limits)?;
    }
    admission_journal::initialize(tx)?;
    insert_body(
        tx,
        "investigation_responses",
        id,
        &response.step_id,
        response,
        4096,
    )?;
    let entry = match reason {
        admission_journal::RefusalReason::Capacity(reason) => {
            admission_journal::append(tx, task, call, response, proposal, reason)?
        }
        reason => admission_journal::append_reason(tx, task, call, response, proposal, reason)?,
    };
    let expected_feedback = entry
        .action_feedback()
        .map(|feedback| feedback.stamp_hash())
        .transpose()?;
    drop(entry);
    // Clone through the bounded canonical record so any failure leaves the
    // caller's last acknowledged memory untouched as its Tx rolls back.
    let mut next: StoredTask =
        serde_json::from_slice(&expected).map_err(|_| InvestigationStoreError::Invalid)?;
    next.detail.usage = usage;
    next.phase = Phase::Ready;
    next.pending = None;
    next.approved = false;
    next.action_tool = None;
    next.admission_state
        .as_mut()
        .ok_or(InvestigationStoreError::Invalid)?
        .current_call = None;
    bump(tx, &mut next)?;
    event(
        tx,
        &mut next,
        InvestigationEventKind::ModelCompleted,
        Some(&response.step_id),
        None,
        "A complete provider response was recorded; no tool was started.",
    )?;
    let (kind, summary) = match reason {
        admission_journal::RefusalReason::Capacity(_) => (
            InvestigationEventKind::CapacityRefused,
            "The proposed acquisition could not be admitted within the proven continuation capacity.",
        ),
        admission_journal::RefusalReason::Action(
            agents::investigation::ActionRefusalReason::RepeatedRead,
        ) => (
            InvestigationEventKind::ActionRefused,
            "This exact read was already acquired; the attempt was charged without executing it again.",
        ),
        admission_journal::RefusalReason::Action(_) => (
            InvestigationEventKind::ActionRefused,
            "The proposed acquisition was not offered in this saved call.",
        ),
    };
    event(tx, &mut next, kind, Some(&response.step_id), None, summary)?;
    // Journal replay validates that no tool event, attempt, or input publication
    // was invented, and derives the count/feedback rather than trusting a slot.
    let state = admission_journal::derive_current(tx, &next)?;
    if let Some(expected) = expected_feedback
        && state
            .action_feedback
            .as_ref()
            .map(|feedback| feedback.stamp_hash())
            .transpose()?
            != Some(expected)
    {
        return Err(InvestigationStoreError::Invalid);
    }
    storage::save_next_prompt_task(tx, &next, task.detail.summary.revision, false)?;
    #[cfg(any(test, feature = "test-support"))]
    if FAIL_AFTER_REFUSAL_WRITE.with(|failure| failure.replace(false)) {
        // Injected failure after the final write and real capacity check, before
        // the caller can commit or acknowledge. This is not natural exhaustion.
        return Err(InvestigationStoreError::Capacity);
    }
    Ok(next)
}

#[cfg(any(test, feature = "test-support"))]
#[path = "admission_transition_tests.rs"]
mod tests;

#[cfg(any(test, feature = "test-support"))]
#[path = "admission_owner_tests.rs"]
mod owner_tests;
