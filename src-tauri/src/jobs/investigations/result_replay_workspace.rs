//! Saved @10 result acquisition/validation, separate from staged task replay.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Retained result and peak including the borrowed task. Conservative duplicate
/// owners are intentional; this is not a tight minimum, native memory or RSS.
pub fn next_prompt_result_workspace() -> Option<(usize, usize)> {
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
        // Result/step fields have at most one tagged layer. AttemptEntry's
        // ReadIntent -> TaskFactKey is the deepest shared row path (two).
        // Malformed JSON depth does not multiply whole Content-tree owners.
        record_decode_scratch_bytes()?.max(json_bytes(MAX_RECORD_BYTES)?.checked_mul(2)?),
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
    let (basis, basis_peak) = completion_basis_workspace()?;
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
        // Selected authority survives result/assessment checks after replay drops.
        basis,
        read.max(next_prompt_ledger_workspace()?)
            .max(basis_peak)
            .max(step_basis)
            .max(completion)
            .max(checks),
    ])?;
    Some((row, peak))
}

/// Selected saved call plus canonical contract/choices/feedback; no replay cache.
pub fn completion_basis_workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let extra = sum(&[
        size_of::<admission_journal::AuthenticatedCompletionBasis>(),
        json_bytes(policy.envelope_bytes().checked_add(256)?)?,
        // Final choices and feedback each fit the existing action serializer.
        json_bytes(MAX_ACTION_BYTES)?.checked_mul(2)?,
        agents::investigation::acquisition_choices::AcquisitionChoices::private_workspace_bytes(
            policy,
        )?,
    ])?;
    let retained = sum(&[
        saved_call_workspace::next_prompt_raw_call_workspace()?.0,
        metadata_history_workspace::row_workspace()?,
        extra,
    ])?;
    // Replay already covers shared history + timeline/entries + transient
    // historical-ledger map/candidate construction in read_entries. Selecting
    // the named call moves its core, rather than retaining a second whole epoch.
    Some((
        retained,
        next_prompt_capacity_replay_workspace()?
            .1
            .checked_add(extra)?,
    ))
}
