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
#[cfg(any(test, feature = "test-support"))]
pub(in super::super) fn project_observation(
    connection: &Connection,
    before: &StoredTask,
    next: &mut StoredTask,
    suffix: &[AttemptEntry],
) -> Result<(), InvestigationStoreError> {
    observation_from_prefix(
        connection,
        before,
        next,
        attempts(connection, before)?,
        suffix,
    )
}

/// A single transaction-local operation: callers can derive a private suffix
/// from the validated count, but cannot supply or retain an arbitrary prefix.
/// The suffix builder must be pure; it receives no prefix/SQL authority.
pub(in super::super) fn project_observation_with_offset(
    connection: &Connection,
    before: &StoredTask,
    next: &mut StoredTask,
    build_suffix: impl FnOnce(&StoredTask, usize) -> Result<Vec<AttemptEntry>, InvestigationStoreError>,
) -> Result<(usize, Vec<AttemptEntry>), InvestigationStoreError> {
    if connection.is_autocommit() {
        return Err(InvestigationStoreError::Invalid);
    }
    #[cfg(any(test, feature = "test-support"))]
    if REREAD_PREFIX.with(|flag| flag.get()) {
        let offset = attempts(connection, before)?.len();
        let suffix = build_suffix(next, offset)?;
        project_observation(connection, before, next, &suffix)?;
        return Ok((offset, suffix));
    }
    let entries = attempts(connection, before)?;
    let offset = entries.len();
    let suffix = build_suffix(next, offset)?;
    observation_from_prefix(connection, before, next, entries, &suffix)?;
    Ok((offset, suffix))
}

fn observation_from_prefix(
    connection: &Connection,
    before: &StoredTask,
    next: &mut StoredTask,
    mut entries: Vec<AttemptEntry>,
    suffix: &[AttemptEntry],
) -> Result<(), InvestigationStoreError> {
    entries.extend_from_slice(suffix);
    let refusals = raw_refusals(connection, before)?;
    let detail = observation_from_entries(next, &entries, &refusals)?;
    next.detail.summary.obligation_status =
        Some(detail.validate_with_policy(&saved_policy(next)?)?);
    next.detail.completion = Some(detail);
    Ok(())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! { static REREAD_PREFIX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(any(test, feature = "test-support"))]
pub(in super::super) fn with_observation_prefix_reread<T>(read: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            REREAD_PREFIX.with(|flag| flag.set(self.0));
        }
    }
    let restore = Restore(REREAD_PREFIX.with(|flag| flag.replace(true)));
    let result = read();
    drop(restore);
    result
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
