//! Pure saved-result validation. Store replay and borrowed DTOs are separate.
use super::staged_finish::ObligationObservation;
use super::*;
use context_hub::logical::{json_bytes, set, slots, sum};
use std::mem::size_of;

/// Additional owners in RefusalRecord::validate_stored_basis_with_policy.
/// Caller owns the current/previous records and ledger, decoded through the
/// strict record preflight. `record_bytes` bounds compact wire for BOTH records.
/// Store acquisition, assessment derivation and native/RSS costs are separate.
pub fn stored_refusal_validation_workspace(
    policy: &InvestigationPolicy,
    ledger_bytes: usize,
    record_bytes: usize,
) -> Option<usize> {
    if ledger_bytes > MAX_RECORD_BYTES || record_bytes > MAX_RECORD_BYTES {
        return None;
    }
    let envelope = sum(&[
        size_of::<ObligationEnvelope>(),
        json_bytes(policy.envelope_bytes())?,
    ])?;
    // content_id clones before assessment-count validation. The previous
    // record's clone can overlap reconstructed feedback. Strict preflight
    // bounds the combined dynamic element count even for malformed records.
    let element = size_of::<ObligationObservation>()
        .max(size_of::<staged_finish::FeedbackEntry>())
        .max(size_of::<acquisition_choices::CitationCorrectionEntry>());
    let cloned = sum(&[
        size_of::<staged_refusal::RefusalRecord>(),
        record_bytes,
        element.checked_mul(RECORD_MAX_VALUES)?,
    ])?;
    let feedback = sum(&[
        slots::<staged_finish::FeedbackEntry>(policy.max_completion_obligations())?,
        slots::<Vec<staged_finish::FeedbackEntry>>(1)?,
        record_bytes, // reconstructed obligation_id/kind strings
    ])?;
    let wire = record_bytes
        .max(MAX_MANIFEST_BYTES)
        .max(policy.envelope_bytes());
    let content_id = sum(&[
        cloned,
        wire.checked_mul(2)?, // encoded record plus domain-framed copy
        b"cartograph:obligation-refusal:v1\0".len(),
        slots::<Vec<u8>>(2)?,
        64,
        slots::<String>(1)?,
    ])?;
    sum(&[
        envelope,
        feedback,
        acquisition_choices::AuthenticatedCitationCorrection::projection_workspace_bytes(policy)?,
        content_id
            .max(super::task_validation_workspace::envelope_workspace(
                policy,
            )?)
            .max(ledger_validation_workspace(policy, ledger_bytes)?)
            .max(disclosure_validation_workspace(policy, ledger_bytes)?),
    ])
}

/// Additional logical owners of validate_completion_for_binding_with_policy.
/// Callers must admit compact typed result/ledger wire before entry, including
/// malformed rows. This excludes caller DTOs, native/regex memory and RSS.
pub fn completion_result_validation_workspace(
    policy: &InvestigationPolicy,
    ledger_bytes: usize,
    result_bytes: usize,
) -> Option<usize> {
    if ledger_bytes > MAX_RECORD_BYTES || result_bytes > MAX_RECORD_BYTES {
        return None;
    }
    let citations = policy
        .limits()
        .selected_facts
        .checked_add(policy.limits().evidence_items)?;
    let obligations = policy.max_completion_obligations();
    let attempts = policy.max_completed_attempts();
    let envelope = super::task_validation_workspace::envelope_workspace(policy)?;
    let disclosure = disclosure_validation_workspace(policy, ledger_bytes)?;
    // targets_with_policy bounds assessment wire before extracting targets.
    // Keep extracted, normalized and fingerprint-normalized target layouts.
    let targets = sum(&[
        json_bytes(MAX_RESULT_BYTES)?,
        json_bytes(policy.envelope_bytes())?.checked_mul(2)?,
        slots::<InvestigationInspectionTarget>(obligations)?.checked_mul(3)?,
        envelope,
    ])?;
    let inspection = sum(&[
        targets,
        set::<&String>(result_bytes)?, // malformed finding citation IDs
        slots::<&InvestigationCitation>(citations)?,
        slots::<Vec<&InvestigationCitation>>(1)?,
        slots::<InvestigationInspectionObservation>(obligations)?,
        obligations.checked_mul(64)?, // cloned target IDs
        size_of::<InvestigationInspectionAssessment>(),
        4 * 128, // retained requirement/ledger/disclosure hashes and step
        disclosure,
    ])?;
    let wire = MAX_RESULT_BYTES
        .max(MAX_MANIFEST_BYTES)
        .max(MAX_DISCLOSURE_BYTES)
        .max(MAX_INPUT_BYTES);
    let hashes = sum(&[wire.checked_mul(2)?, slots::<Vec<u8>>(2)?, 256])?;
    // content_id clones the whole result before its bounded encoding.
    let result_copy = sum(&[size_of::<InvestigationResult>(), json_bytes(result_bytes)?])?;
    // validate_bound clones findings into Finish before shape admission.
    // Once admitted, at most 12 findings and eight limitations per list remain.
    let finish = sum(&[
        size_of::<InvestigationAction>(),
        json_bytes(result_bytes)?,
        slots::<InvestigationFinding>(12)?,
        json_bytes(result_bytes)?,
        12 * "finding-12".len(),
        slots::<Vec<InvestigationFinding>>(1)?,
        slots::<&str>(12 * (2 + 8) + 8 + 2)?,
        MAX_INPUT_BYTES,
        slots::<String>(1)?,
        slots::<char>(MAX_INPUT_BYTES)?,
        slots::<Vec<char>>(1)?,
        set::<&[char]>(MAX_INPUT_BYTES)?,
        // Each admitted prose item is <=2048 bytes. Retain consecutive
        // redaction outputs; supplied strings are empty in saved validation.
        core_redact::direct_source_json_content_bound(2048)?.checked_mul(2)?,
        slots::<String>(2)?,
    ])?;
    let result_phase = sum(&[
        inspection,
        ledger_validation_workspace(policy, ledger_bytes)?,
        set::<String>(citations)?,
        ledger_bytes, // eligible citation IDs
        set::<&String>(result_bytes)?,
        set::<&String>(12)?, // narrative finding membership
        64,
        slots::<String>(1)?,
        result_copy.max(finish),
        hashes,
    ])?;
    // Completion validation keeps its normalized envelope and cloned contract
    // while derive_assessment normalizes again and retains source assessment.
    let contract_phase = sum(&[
        envelope.checked_mul(3)?,
        size_of::<InvestigationCompletionContract>(),
        inspection,
        attempt_binding_validation_workspace(policy, ledger_bytes)?,
        slots::<&str>(attempts)?,
        set::<&&str>(attempts)?,
        slots::<&staged_attempt::AttemptEvidence<'_>>(attempts)?,
        slots::<&DisclosedInvestigationSource>(citations)?,
        set::<usize>(usize::try_from(policy.limits().tool_actions).ok()?)?,
        slots::<&DisclosedInvestigationCoverageDetail>(
            usize::try_from(policy.limits().tool_actions).ok()?,
        )?,
        slots::<ObligationObservation>(obligations)?,
        obligations.checked_mul(64 + "inventory".len())?,
        slots::<Vec<ObligationObservation>>(1)?,
        256,
        hashes,
    ])?;
    Some(result_phase.max(contract_phase))
}
