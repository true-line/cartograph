//! Atomic capacity-refusal effects inside the caller's authenticated writer Tx.
//! The caller verifies execution ownership/cancellation and commits or rolls
//! back. This internal body grants no dispatch authority or public @10 route.
use super::*;
use agents::investigation::staged_refusal::CompletionCallMode;
use rusqlite::Transaction;
use serde_json::Value;

pub(super) fn refuse_capacity(
    tx: &Transaction<'_>,
    task: &StoredTask,
    response: &InvestigationResponseMeta,
    proposal: &Value,
    reason: CapacityRefusalReason,
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
    let saved: (Vec<u8>, String) = tx.query_row(
        "SELECT CASE WHEN typeof(record)='blob' AND length(record)<=262144 THEN record END, record_hash FROM investigation_tasks WHERE id=?1 AND revision=?2",
        params![id,signed(task.detail.summary.revision)?],
        |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional()?.ok_or(InvestigationStoreError::Stale)?;
    if saved.0 != expected || saved.1 != core_prov::content_hash(&saved.0) {
        return Err(InvestigationStoreError::Stale);
    }
    let call = task
        .admission_state
        .as_ref()
        .and_then(|state| state.current_call.as_ref())
        .ok_or(InvestigationStoreError::Invalid)?;
    call.validate_saved(tx, task)?;
    if call.mode() != CompletionCallMode::Choice {
        // Worker settles the distinct terminal protocol outcome. A refusal here
        // must never create feedback or permit another model call.
        return Err(InvestigationStoreError::Invalid);
    }
    let (_, usage) = transitions::validate_response(tx, task, response)?;
    admission_journal::initialize(tx)?;
    insert_body(
        tx,
        "investigation_responses",
        id,
        &response.step_id,
        response,
        4096,
    )?;
    admission_journal::append(tx, task, call, response, proposal, reason)?;
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
    event(
        tx,
        &mut next,
        InvestigationEventKind::CapacityRefused,
        Some(&response.step_id),
        None,
        "The proposed acquisition could not be admitted within the proven continuation capacity.",
    )?;
    // Journal replay validates that no tool event, attempt, or input publication
    // was invented, and derives the count/feedback rather than trusting a slot.
    admission_journal::derive_current(tx, &next)?;
    storage::save_next_prompt_task(tx, &next, task.detail.summary.revision, false)?;
    Ok(next)
}

#[cfg(test)]
#[path = "admission_transition_tests.rs"]
mod tests;
