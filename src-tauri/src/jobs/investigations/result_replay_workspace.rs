//! Saved @10 result acquisition/validation, separate from staged task replay.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Retained result and peak including the borrowed task. Conservative duplicate
/// owners are intentional; this is not a tight minimum, native memory or RSS.
pub(crate) fn next_prompt_result_workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    // Raw result/step rows can fail semantic limits after decoding. Each
    // dynamic element consumes a preflight JSON value; no allocating defaults.
    let element = size_of::<InvestigationFinding>()
        .max(size_of::<InvestigationInspectionObservation>())
        .max(size_of::<ObligationObservation>());
    let row = sum(&[
        metadata_history_workspace::row_workspace()?,
        size_of::<InvestigationResult>(),
        element.checked_mul(RECORD_MAX_VALUES)?,
        json_bytes(MAX_RECORD_BYTES)?,
    ])?;
    let read = sum(&[
        row,
        record_decode_scratch_bytes()?
            .max(json_bytes(MAX_RECORD_BYTES)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?),
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        128,
        slots::<String>(3)?,
        4096,
    ])?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (ledger, decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let ledger_read = sum(&[decode, MAX_RECORD_BYTES, slots::<Vec<u8>>(1)?, 128, 4096])?;
    let semantic = agents::investigation::completion_result_validation_workspace(
        policy,
        MAX_RECORD_BYTES,
        MAX_RECORD_BYTES,
    )?;
    // validate_step_basis retains a SECOND ledger and obligation sources while
    // validate_step_metadata replays the saved call and loads another core.
    let step_basis = sum(&[
        ledger,
        json_bytes(policy.envelope_bytes())?,
        next_prompt_contract_workspace()?,
        next_prompt_ledger_workspace()?
            .max(next_prompt_saved_validation_workspace()?.0)
            .max(next_prompt_saved_call_workspace()?.1)
            .max(agents::investigation::disclosure_validation_workspace(
                policy,
                MAX_RECORD_BYTES,
            )?),
    ])?;
    let count = policy.max_completed_attempts();
    // validate_result first retains attempts, then collects EVERY historical
    // ledger before validating completion binding. Include malformed ledgers.
    let completion = sum(&[
        row.checked_mul(count)?,
        slots::<Vec<completion::AttemptEntry>>(1)?,
        ledger.checked_mul(count)?,
        slots::<Vec<InvestigationInputLedger>>(1)?,
        slots::<agents::investigation::staged_attempt::AttemptEvidence<'_>>(count)?,
        slots::<Vec<agents::investigation::staged_attempt::AttemptEvidence<'_>>>(1)?,
        next_prompt_attempt_replay_workspace()?
            .max(ledger_read)
            .max(semantic),
    ])?;
    let checks = sum(&[
        semantic,
        next_prompt_contract_workspace()?,
        MAX_INVOCATION_BYTES.max(MAX_DISCLOSURE_BYTES),
        slots::<Vec<u8>>(1)?,
    ])?;
    let peak = sum(&[
        task,
        row,
        ledger,
        row, // result, current ledger, named step
        read.max(next_prompt_ledger_workspace()?)
            .max(step_basis)
            .max(completion)
            .max(checks),
    ])?;
    Some((row, peak))
}
