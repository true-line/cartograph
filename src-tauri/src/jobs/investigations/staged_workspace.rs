//! Logical ownership of staged::load_for_task for @10/V2, not outer task replay.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Retained journal vector and peak including the borrowed task. No native,
/// allocator/RSS or legacy-family claim. All row owners use strict preflight
/// caps before semantic validation; successful-state counts alone are unsafe.
pub fn next_prompt_staged_workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let (task, task_decode) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let row = metadata_history_workspace::row_workspace()?;
    let (ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let count = policy.max_completed_attempts();
    let calls = usize::try_from(policy.limits().model_invocations).ok()?;
    let retained = sum(&[row.checked_mul(2)?, slots::<Vec<JournalEntry>>(1)?])?;
    // Binding's dynamic envelope is a subset of StoredTask's closed DTO union.
    let binding = task.checked_add(size_of::<Binding>())?;
    // Historical factory reloads the complete Entry and ordinary response
    // sequentially, dropping both before history/timeline. This bounded row +
    // decode term covers rejected bodies too; their encodings are not retained.
    let acquisition = sum(&[
        row,
        task_decode.max(ledger_decode), // includes malformed tagged content
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        slots::<String>(3)?,
        128,
        4096, // fixed SQL and key formatting
    ])?;
    // metadata_at_saved_step retains metadata while validate_core reloads its
    // witnesses; the constructed SavedCallBasis/admission survives derive_at.
    let (_, metadata, metadata_peak) = next_prompt_metadata_workspace()?;
    let saved_basis = sum(&[
        row,  // cloned admission from the bounded invocation
        task, // SavedCallBasis identifiers cloned from the admitted task
        size_of::<admission_contract::SavedCallBasis>(),
        metadata,
        slots::<&String>(RECORD_MAX_VALUES)?, // entry list before attempt-count validation
        slots::<&String>(count)?,
        slots::<Vec<&String>>(2)?,
        metadata_peak
            .max(next_prompt_saved_call_workspace()?.1)
            .max(next_prompt_capacity_replay_workspace()?.1),
    ])?;
    // completion::attempts authenticates a second vector before collecting all
    // historical ledgers. Keep it alongside those ledgers, not sequentially.
    let witnesses = sum(&[
        row.checked_mul(count)?,
        slots::<Vec<completion::AttemptEntry>>(1)?,
        ledger.checked_mul(count)?,
        slots::<Vec<InvestigationInputLedger>>(1)?,
        slots::<&AttemptRecord>(count)?.checked_mul(2)?,
        slots::<Vec<&AttemptRecord>>(2)?,
        slots::<AttemptEvidence<'_>>(count)?,
        slots::<Vec<AttemptEvidence<'_>>>(1)?,
    ])?;
    // The completion validator's contract phase includes assessment derivation.
    // Its returned assessment remains live through every later check below.
    let assessment = sum(&[
        slots::<ObligationObservation>(policy.max_completion_obligations())?,
        slots::<Vec<ObligationObservation>>(1)?,
        MAX_RECORD_BYTES, // obligation/kind text copied from admitted envelope
    ])?;
    let semantic =
        completion_result_validation_workspace(policy, MAX_RECORD_BYTES, MAX_RECORD_BYTES)?.max(
            stored_refusal_validation_workspace(policy, MAX_RECORD_BYTES, MAX_RECORD_BYTES)?,
        );
    // JournalEntry identity has its own Value and domain-framed encoding;
    // RefusalRecord's clone/identity helper does not cover this enclosing DTO.
    let identity = sum(&[
        json_bytes(MAX_RECORD_BYTES)?,
        MAX_RECORD_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
        256,
        slots::<String>(2)?,
        // Independent feedback witness framing follows record identity; charge
        // both encodings conservatively rather than relying on phase reuse.
        MAX_MANIFEST_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
        64,
    ])?;
    // Ordered invocation DTOs coexist with the new step-ID vector during
    // collection; the response sweep keeps the prior response during next read.
    let usage = sum(&[
        row.checked_mul(calls.checked_add(2)?)?,
        slots::<Vec<(u32, PendingInvestigationStep)>>(1)?,
        slots::<(u32, PendingInvestigationStep)>(calls)?,
        slots::<Option<String>>(calls)?,
        slots::<Vec<Option<String>>>(1)?,
        calls.checked_mul(128)?,
        metadata_peak.max(acquisition).max(4096),
    ])?;
    let (selected, selected_peak) = result_replay_workspace::completion_basis_workspace()?;
    let peak = sum(&[
        task,
        binding,
        retained,
        row.checked_mul(2)?, // feedback and current named invocation
        ledger,
        witnesses,
        assessment,
        selected, // historical named call retained through record/usage checks
        schema_workspace()?
            .max(selected_peak)
            .max(acquisition)
            .max(next_prompt_contract_workspace()?)
            .max(next_prompt_ledger_workspace()?)
            .max(saved_basis)
            .max(next_prompt_attempt_replay_workspace()?)
            .max(semantic)
            .max(identity)
            .max(usage),
    ])?;
    Some((retained, peak))
}

fn schema_workspace() -> Option<usize> {
    let expected = schema();
    let count = expected.len().checked_add(1)?;
    let bytes = expected.iter().map(String::len).max()?.checked_mul(count)?;
    // validate_schema retains expected SQL, collects Option rows, converts to
    // Strings, then sorts. Reserve both collecting vectors and sort scratch.
    sum(&[
        bytes.checked_mul(2)?,
        slots::<Option<String>>(count)?,
        slots::<String>(count)?.checked_mul(3)?,
        slots::<Vec<String>>(2)?,
        slots::<Vec<Option<String>>>(1)?,
    ])
}
