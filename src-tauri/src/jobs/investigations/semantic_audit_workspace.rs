//! Audit receipt replay ownership. This does not cover provider dispatch or
//! compact-prompt construction, and is not an allocator/native/RSS estimate.
// Every workspace bound here is Unix-only (#227).
#[cfg(unix)]
use super::*;
#[cfg(unix)]
use agents::investigation::semantic_audit::{AuditSubject, SemanticAuditAccount, SubjectReview};
#[cfg(unix)]
use context_hub::logical::{json_bytes, set, slots, sum};
#[cfg(unix)]
use std::mem::size_of;

#[cfg(unix)]
pub(super) fn receipt_replay_workspace() -> Option<usize> {
    // SQL admits MAX_RECORD_BYTES before typed decode. A malformed receipt can
    // contain more reviews than semantic validation accepts; each requires a
    // distinct preflight JSON value. Tagged subject/verdict strings are inside
    // SubjectReview; no allocating defaults or extra containers are synthesized.
    let receipt = sum(&[
        size_of::<super::semantic_audit_receipt::AuditReceipt>(),
        MAX_RECORD_BYTES,
        slots::<SubjectReview>(RECORD_MAX_VALUES)?,
    ])?;
    let invocation = sum(&[
        size_of::<super::semantic_audit_journal::AuditInvocation>(),
        MAX_RECORD_BYTES,
    ])?;
    let retained = receipt.checked_add(invocation)?;
    // Two tagged child DTOs in the receipt; conservatively retain both Content
    // trees through decoding, alongside the SQL row and canonical-decode scratch.
    let read = sum(&[
        retained,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        record_decode_scratch_bytes()?,
        json_bytes(MAX_RECORD_BYTES)?.checked_mul(2)?,
        4096, // the existing bounded row-query/scalar envelope
    ])?;
    let validation = sum(&[
        retained,
        // Public advisory projection is compared against the immutable receipt.
        MAX_RECORD_BYTES,
        slots::<SubjectReview>(RECORD_MAX_VALUES)?,
        MAX_RECORD_BYTES,   // receipt re-encoding before semantic admission
        MAX_MANIFEST_BYTES, // invocation fingerprint
        MAX_ACTION_BYTES,   // typed reply bound
        MAX_RESULT_BYTES,   // candidate fingerprint survives subject validation
        slots::<Vec<u8>>(4)?,
        set::<AuditSubject>(RECORD_MAX_ARRAY_ITEMS)?,
        set::<&AuditSubject>(RECORD_MAX_ARRAY_ITEMS)?,
        slots::<std::collections::BTreeSet<AuditSubject>>(2)?,
        slots::<SemanticAuditAccount>(2)?,
        MAX_RECORD_BYTES, // original-limit fingerprint serialization
    ])?;
    // validate_saved owns cloned source IDs throughout receipt loading and the
    // final published-result replay; caller task/ledger/basis remain external.
    let ids = sum(&[
        MAX_RECORD_BYTES,
        set::<String>(RECORD_MAX_VALUES)?,
        slots::<std::collections::BTreeSet<String>>(1)?,
    ])?;
    ids.checked_add(
        read.max(validation)
            .max(retained.checked_add(next_prompt_result_workspace()?.1)?),
    )
}

/// Durable preparation/start/settlement above the primitive replay components.
/// Caller original input/provider owners and the live audit provider owners are
/// separate. Source validation is supplied by the host, never inferred from a
/// stored byte counter. Provider limits come from the caller so this store layer
/// also compiles in the native ownership harness without the application host.
/// Nested task replay includes its own second task copy.
#[cfg(unix)]
pub fn next_prompt_audit_transaction_workspace(
    source_validation: usize,
    limits: &llm::bounded::CompletionLimits,
) -> Option<usize> {
    let policy = &INVESTIGATION_V2_POLICY;
    let (task, task_load) = next_prompt_task_load_workspace()?;
    let (ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (result, result_peak) = next_prompt_result_workspace()?;
    let (basis, basis_peak) = completion_basis_workspace()?;
    let (projection, projection_peak) =
        agents::investigation::semantic_audit::projection_workspace_bytes()?;
    let (_, seal) = InvestigationInput::sealing_workspace_bytes(policy, limits)?;
    // prepare_payload_for_task retains its step and acquired ledger through
    // sealing and compact construction. result's row envelope covers the step's
    // disclosed dynamic fields as well as malformed row decoding.
    let ids = sum(&[
        MAX_RECORD_BYTES,
        set::<String>(RECORD_MAX_VALUES)?,
        slots::<std::collections::BTreeSet<String>>(1)?,
    ])?;
    let reconstruct = sum(&[
        ledger,
        result,
        ids,
        ledger_decode
            .max(result_peak)
            .max(seal)
            .max(projection_peak),
        agents::investigation::ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
    ])?;
    let invocation = sum(&[
        size_of::<super::semantic_audit_journal::AuditInvocation>(),
        MAX_RECORD_BYTES,
    ])?;
    let receipt = sum(&[
        size_of::<super::semantic_audit_receipt::AuditReceipt>(),
        MAX_RECORD_BYTES,
        slots::<SubjectReview>(RECORD_MAX_VALUES)?,
    ])?;
    // A returned expected projection survives action matching; action() clones
    // its payload, and may retain its existing opaque 128-byte action ID.
    let expected_action = sum(&[projection, size_of::<llm::CompletionAction>(), 128])?;
    let authenticate = sum(&[
        projection,
        expected_action,
        invocation,
        receipt,
        ids,
        // Reply preflight and internally tagged subject/verdict Content trees
        // exist before closed-schema/binding validation can reject the reply.
        record_decode_scratch_bytes()?,
        json_bytes(MAX_ACTION_BYTES)?.checked_mul(2)?,
        receipt_replay_workspace()?,
    ])?;
    // Once authenticate returns, its expected projection drops. The settlement
    // retains receipt, acquired ledger, cloned candidate and IDs until commit;
    // a selected basis/step survives completion::accepted on the pass branch.
    let publish = sum(&[
        receipt,
        // Settlement clones receipt reviews into saved public task detail.
        MAX_RECORD_BYTES,
        slots::<SubjectReview>(RECORD_MAX_VALUES)?,
        ledger,
        result,
        ids,
        basis,
        result,
        basis_peak
            .max(result_peak)
            .max(receipt_replay_workspace()?)
            .max(next_prompt_task_write_workspace()?)
            .max(task_load),
        MAX_RESULT_BYTES,
        MAX_RECORD_BYTES,
        MAX_MANIFEST_BYTES,
        slots::<Vec<u8>>(3)?,
        4096, // result/event/job and scalar SQL envelopes
    ])?;
    let begin = sum(&[
        projection,
        invocation,
        source_validation
            .max(next_prompt_task_write_workspace()?)
            .max(task_load),
        MAX_MANIFEST_BYTES,
        slots::<Vec<u8>>(1)?,
    ])?;
    task.checked_add(
        reconstruct
            .max(authenticate)
            .max(publish)
            .max(begin)
            .max(task_load),
    )
}
