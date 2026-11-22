//! Durable source projection and writer ownership; borrowed host context is separate.
use super::*;
use context_hub::logical::{slots, sum};
use std::mem::size_of;

/// Returns projection currency and writer peak. Callers supply the actual
/// continuation verification peak, candidate prepared/disclosure retention,
/// and retained-source callback peak. No provider execution or RSS is modeled.
pub fn workspace(
    continuation_peak: usize,
    continuation_retained: usize,
    callback_peak: usize,
) -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let (task, task_load) = next_prompt_task_load_workspace()?;
    let (saved, saved_load) =
        read_reservations::workspace::next_prompt_read_reservation_workspace()?;
    let (core, core_load) = next_prompt_saved_call_workspace()?;
    let (loaded_ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let ledger = sum(&[
        loaded_ledger,
        size_of::<InvestigationCitation>(),
        MAX_INSPECTION_TARGET_BYTES,
        64,
        "evidence-"
            .len()
            .checked_add(usize::MAX.to_string().len())?,
    ])?;
    let (_, metadata, metadata_load) = next_prompt_metadata_workspace()?;
    let row = metadata_history_workspace::row_workspace()?;
    let attempts = sum(&[
        slots::<Vec<completion::AttemptEntry>>(1)?,
        slots::<completion::AttemptEntry>(policy.max_completed_attempts())?,
        MAX_RECORD_BYTES.checked_mul(policy.max_completed_attempts())?,
    ])?;
    let events = sum(&[
        slots::<Vec<InvestigationEvent>>(1)?,
        slots::<InvestigationEvent>(2)?,
        MAX_RECORD_BYTES.checked_mul(2)?,
    ])?;
    // Before task encoding, acquired citations and raw refusal feedback can be
    // cloned. Terminal observation additionally derives an assessment from
    // attempts and clones the task's obligation envelope. Keep their complete
    // input currencies, not just accepted task wire or semantic feedback caps.
    let projected_task = sum(&[task.checked_mul(2)?, ledger, row, attempts, 2048])?;
    let projection = sum(&[
        size_of::<SettlementPlan>(),
        projected_task,
        saved, // disposition constructed from saved reservation
        events,
        ledger,
        attempts,
        64, // before_hash
    ])?;
    let response = sum(&[size_of::<InvestigationResponseMeta>(), MAX_RECORD_BYTES])?;
    // validate_continuation retains derived records and appended metadata while
    // sealing/measuring. Project retains before/saved/core/response; persist
    // reloads actual + saved and clones the projected task for observation.
    // Summing these sequential owners is deliberately conservative.
    let stable = sum(&[
        task.checked_mul(2)?,
        saved.checked_mul(2)?,
        core,
        response,
        projection,
        projected_task,
        metadata.checked_mul(2)?,
        // H saved prefix + S derived suffix fit two full attempt currencies
        // (H+S <= policy maximum). Prefix moves into observation, never clones;
        // that observation Vec and the retained suffix remain within these roles.
        attempts.checked_mul(2)?,
        ledger.checked_mul(2)?,
        continuation_retained,
    ])?;
    let constructing_attempt =
        sum(&[size_of::<AttemptRecord>(), task, saved, ledger, response])?.checked_mul(2)?;
    let validation = sum(&[
        constructing_attempt,
        attempt_binding_validation_workspace(policy, MAX_RECORD_BYTES)?,
        next_prompt_contract_workspace()?,
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        continuation_peak,
        next_prompt_metadata_workspace()?.0,
        metadata_history_workspace::acquisition_choices_workspace()?,
    ])?;
    // project_observation/refresh_observation own loaded attempts, raw refusals
    // and a new detail while validating it, including the terminal alternative.
    let observation = sum(&[
        next_prompt_attempt_replay_workspace()?,
        metadata_load,
        projected_task,
        next_prompt_contract_workspace()?,
    ])?;
    // All writer encoders are bounded; comparison keeps two task buffers live.
    // insert_body/persist_input_rows use sequential buffers. Capacity checks
    // and admission_journal::derive_current retain their own replay maps.
    let encoding = sum(&[
        MAX_RECORD_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
        128,
        slots::<String>(2)?,
        8192, // bounded statement/identity formatting
    ])?;
    let peak = stable.checked_add(
        storage::archive_schema_workspace()?
            .max(task_load)
            .max(saved_load)
            .max(core_load)
            .max(ledger_decode)
            .max(next_prompt_pending_source_workspace()?)
            .max(next_prompt_source_capacity_workspace()?.1)
            .max(next_prompt_capacity_replay_workspace()?.1)
            .max(next_prompt_staged_workspace()?.1)
            .max(validation)
            .max(observation)
            .max(encoding)
            .max(callback_peak),
    )?;
    Some((projection, peak))
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn source_settlement_workspace_keeps_caller_phases_and_refuses_overflow() {
        // AC-0327: do not let an unrelated replay maximum hide callback owners
        // or candidate preparation that must survive the whole writer.
        let (_, base) = workspace(0, 0, 0).unwrap();
        assert_eq!(workspace(0, 23, 0).unwrap().1, base + 23);
        assert!(workspace(base, 0, 0).unwrap().1 > base);
        assert!(workspace(0, 0, base).unwrap().1 > base);
        assert!(workspace(usize::MAX, 0, 0).is_none());
        assert!(workspace(0, usize::MAX, 0).is_none());
        assert!(workspace(0, 0, usize::MAX).is_none());
    }
}
