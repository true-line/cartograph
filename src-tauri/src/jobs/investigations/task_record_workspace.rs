//! Closed StoredTask decoding currency, not an arbitrary Deserialize<T> bound.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Retained task and acquisition/decode peak after SQL admits the row byte cap.
/// Covers derived serde's logical content trees, not allocator/native layout or
/// RSS. SQL/cache internals, scalar mirrors, pure validation and nested durable
/// replay are separate. Malformed records are included before family validation.
pub fn next_prompt_record_workspace(bytes: usize) -> Option<(usize, usize)> {
    if bytes > MAX_RECORD_BYTES {
        return None;
    }
    // Exhaustive dynamic element/box types reachable from StoredTask. Inline
    // nested fields are already in the root or element size. Each element or
    // box requires a distinct admitted JSON value; decoded string payload is
    // no larger than the input bytes. No nonempty defaults synthesize payload:
    // optional fields default None, coverage private maps/vectors default empty.
    let element = [
        size_of::<String>(),
        size_of::<InvestigationCitation>(),
        size_of::<InvestigationFinding>(),
        size_of::<agents::investigation::semantic_audit::SubjectReview>(),
        size_of::<context_hub::coverage::RepoQualification>(),
        size_of::<InvestigationInspectionTarget>(),
        size_of::<InvestigationInspectionObservation>(),
        size_of::<Obligation>(),
        size_of::<ObligationObservation>(),
        size_of::<DisclosedInvestigationCoverageDetail>(),
        size_of::<DisclosedInvestigationFact>(),
        size_of::<DisclosedInvestigationQuery>(),
        size_of::<DisclosedInvestigationSource>(),
    ]
    .into_iter()
    .max()?;
    let retained = sum(&[
        size_of::<StoredTask>(),
        bytes,
        element.checked_mul(RECORD_MAX_VALUES)?,
    ])?;
    // Flattened InvestigationDetail and internally tagged DTOs can retain
    // content buffers while decoding children. Reserve a complete admitted
    // logical JSON tree at each possible nesting level, including the root;
    // this deliberately does not depend on serde's private Content layout.
    // McpClientId's custom visitor validates then copies its 32 input bytes;
    // required_feedback delegates to ordinary Option decoding without expansion.
    let content = json_bytes(bytes)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?;
    let decoding = record_decode_scratch_bytes()?.max(content);
    let acquisition = sum(&[bytes, slots::<Vec<u8>>(1)?, 64, slots::<String>(1)?])?;
    Some((retained, sum(&[acquisition, retained, decoding])?))
}

/// Complete load_next_prompt_task peak, including its returned task. The
/// caller owns the transaction, indexed access check and outer schema preflight.
/// Only @10/V2 is covered; native memory and detached-worker admission are not.
#[cfg(unix)]
pub fn next_prompt_task_load_workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let (task, decode) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let row = metadata_history_workspace::row_workspace()?
        .checked_add(size_of::<completion::AttemptEntry>())?;
    // Row/scalar mirrors live through pure validation, then are explicitly
    // dropped before pending-state/ledger/result replay in the base loader.
    let mirrors = sum(&[128 * 2 + 36 * 2 + 32, slots::<String>(5)?, 4096])?;
    let pure = task.checked_add(next_prompt_contract_workspace()?)?;
    let (saved, consent) = next_prompt_saved_validation_workspace()?;
    let (result, result_peak) = next_prompt_result_workspace()?;
    let base = mirrors.checked_add(decode.max(pure))?.max(
        next_prompt_pending_source_workspace()?
            .max(saved)
            .max(consent)
            .max(next_prompt_ledger_workspace()?)
            .max(result_peak),
    );
    let (journal, staged_peak) = next_prompt_staged_workspace()?;
    // validate_saved retains attempts AND raw refusals while load_for_task
    // acquires a second validated refusal vector. Both survive later checks.
    let witnesses = sum(&[
        row.checked_mul(policy.max_completed_attempts())?,
        slots::<Vec<completion::AttemptEntry>>(1)?,
        journal.checked_mul(2)?,
    ])?;
    let (ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    // The result branch reloads result, step and ledger, then constructs an
    // expected completion detail (cloned envelope/assessment/identity strings).
    // Bound the clone from untrusted row currency, not successful field counts.
    let expected = sum(&[
        size_of::<InvestigationCompletionDetail>(),
        task,
        result,
        row,
        slots::<&str>(policy.max_completed_attempts())?,
        slots::<Vec<&str>>(1)?,
    ])?;
    let acquisition = sum(&[
        row,
        decode.max(ledger_decode),
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        4096,
        128,
        slots::<String>(3)?,
    ])?;
    let (basis, basis_peak) = result_replay_workspace::completion_basis_workspace()?;
    let projection = sum(&[
        result,
        row,
        ledger,
        expected,
        basis,
        acquisition
            .max(basis_peak)
            .max(next_prompt_ledger_workspace()?)
            .max(completion_result_validation_workspace(
                policy,
                MAX_RECORD_BYTES,
                MAX_RECORD_BYTES,
            )?)
            .max(next_prompt_contract_workspace()?),
    ])?;
    let completion = sum(&[
        task,
        witnesses,
        next_prompt_attempt_replay_workspace()?
            .max(staged_peak)
            .max(acquisition) // raw_refusals before staged semantic validation
            .max(projection),
    ])?;
    // A pending audit retains its candidate in the task while validating the
    // saved finishing disclosure, then reconstructs model-authored fields as a
    // canonical JSON action. Count both typed and JSON representations and
    // the bounded hashing/encoding buffers; no raw source payload is retained.
    let audit = sum(&[
        task,
        projection,
        super::semantic_audit_workspace::receipt_replay_workspace()?,
        json_bytes(MAX_RESULT_BYTES)?.checked_mul(2)?,
        json_bytes(MAX_ACTION_BYTES)?.checked_mul(2)?,
        MAX_RESULT_BYTES,
        MAX_ACTION_BYTES,
    ])?;
    Some((task, base.max(completion).max(audit)))
}

/// Logical owners of the task3 writer, including its borrowed caller task.
/// Audit accounting keeps a complete replacement alive through validation and
/// encoding. Historical writes use less memory; reserve the audited path before
/// enabling it. Transaction/native SQLite storage is admitted separately.
#[cfg(unix)]
pub fn next_prompt_task_write_workspace() -> Option<usize> {
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    sum(&[
        task.checked_mul(2)?,
        next_prompt_contract_workspace()?,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
    ])
}
