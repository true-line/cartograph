//! Shared production DTO construction; never completed-read authority.
use super::*;

pub(in super::super) fn project_attempt_entries(
    records: Vec<AttemptRecord>,
    response: &InvestigationResponseMeta,
    event_sequence: u64,
) -> Result<Vec<AttemptEntry>, InvestigationStoreError> {
    let entries = records
        .into_iter()
        .map(|record| AttemptEntry {
            record,
            response_hash: response.response_hash.clone(),
            action_hash: response.action_hash.clone(),
            event_sequence,
        })
        .collect::<Vec<_>>();
    for entry in &entries {
        encode(entry, MAX_RECORD_BYTES)?;
    }
    Ok(entries)
}

/// Exact observation detail over the actual saved prefix plus a private
/// projected suffix. The caller must separately authenticate/derive the suffix.
/// Reads only: these rows need not already have been published to calculate
/// task-body growth. Existing refresh_observation uses the same pure kernel.
pub(in super::super) fn project_observation(
    connection: &Connection,
    before: &StoredTask,
    next: &mut StoredTask,
    suffix: &[AttemptEntry],
) -> Result<(), InvestigationStoreError> {
    let mut entries = attempts(connection, before)?;
    entries.extend_from_slice(suffix);
    let refusals = raw_refusals(connection, before)?;
    let detail = observation_from_entries(next, &entries, &refusals)?;
    next.detail.summary.obligation_status = Some(detail.validate_with_policy(saved_policy(next)?)?);
    next.detail.completion = Some(detail);
    Ok(())
}

/// Sizing-only mutation of a private projected row, never an authority check.
/// Caller must keep this row private and return bounds only.
pub(in super::super) fn size_attempt_event_sequence(entry: &mut AttemptEntry, sequence: u64) {
    entry.event_sequence = sequence;
}

/// Persist exactly the already bounded private DTOs. This is a row writer,
/// not completed-read authority; its owning transaction authenticates/rederives
/// the suffix and checks its reserved stage allocation and final task CAS.
pub(in super::super) fn persist_projected_entries(
    connection: &Connection,
    id: &str,
    offset: usize,
    entries: &[AttemptEntry],
) -> Result<(), InvestigationStoreError> {
    if connection.is_autocommit()
        || offset
            .checked_add(entries.len())
            .is_none_or(|n| n > INVESTIGATION_V2_POLICY.max_completed_attempts())
    {
        return Err(InvestigationStoreError::Invalid);
    }
    for (index, entry) in entries.iter().enumerate() {
        insert_body(
            connection,
            "completion_attempts",
            id,
            &signed((offset + index + 1) as u64)?,
            entry,
            MAX_RECORD_BYTES,
        )?;
    }
    Ok(())
}
