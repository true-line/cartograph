//! SavedMetadataHistory load/metadata_at owners, not SavedCallCore or RSS.
use super::*;
use agents::investigation::{staged_attempt::AttemptRecord, staged_finish::FeedbackEntry};
use context_hub::logical::{json_bytes, map, set, slots, sum};
use std::mem::size_of;

/// Retained history, additional metadata projection, and complete load/project
/// peak including its borrowed task owner. Schema preflight is caller-owned.
#[cfg(unix)]
pub fn next_prompt_metadata_workspace() -> Option<(usize, usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let calls = usize::try_from(policy.limits().model_invocations).ok()?;
    let attempts = policy.max_completed_attempts();
    let row = row_workspace()?;
    let decoder = record_decode_scratch_bytes()?
        .max(json_bytes(MAX_RECORD_BYTES)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?);
    let acquire = sum(&[
        row,
        decoder,
        MAX_RECORD_BYTES,
        64,
        slots::<Vec<u8>>(1)?,
        slots::<String>(2)?,
        4096, // fixed body-read SQL and hash/key owners
    ])?;
    let steps = sum(&[
        slots::<Vec<(u32, PendingInvestigationStep)>>(1)?,
        row.checked_mul(calls)?,
        slots::<(u32, PendingInvestigationStep)>(calls)?,
    ])?;
    let index = sum(&[map::<String, u32>(calls)?, calls.checked_mul(128)?])?;
    let entries = sum(&[
        slots::<Vec<completion::AttemptEntry>>(1)?,
        slots::<completion::AttemptEntry>(attempts)?,
        attempts.checked_mul(MAX_RECORD_BYTES)?,
        slots::<Vec<staged::JournalEntry>>(1)?,
        row.checked_mul(2)?,
    ])?;
    let (archive, _) = InvestigationCoverageArchive::decode_workspace_bytes()?;
    let history = sum(&[
        steps,
        index,
        entries,
        archive,
        size_of::<completion::SavedMetadataHistory<'_>>(),
    ])?;
    // The SQL sweep admits count and 128-byte keys before collecting. The
    // owning key iterator and accumulated steps overlap each next row decode.
    // Stable sort may allocate another element array. Admission validation
    // borrows scalar fields; validate_step uses bounded sequential encodings.
    let sweep = sum(&[
        slots::<Vec<String>>(1)?,
        slots::<String>(calls)?,
        calls.checked_mul(128)?,
        slots::<(u32, PendingInvestigationStep)>(calls)?,
        acquire.max(MAX_INVOCATION_BYTES.checked_add(MAX_DISCLOSURE_BYTES)?),
    ])?;
    // metadata_at clones IDs and the last refusal's feedback before validating
    // it. Reserve a whole untrusted refusal row for that clone, plus output IDs.
    let metadata = sum(&[
        size_of::<ProtectedCompletionMetadata>(),
        row,
        slots::<String>(attempts)?,
        attempts.checked_mul(MAX_RECORD_BYTES)?,
        MAX_RECORD_BYTES, // copied request/obligation IDs from the admitted task
        slots::<&RefusalRecord>(2)?,
        slots::<Vec<&RefusalRecord>>(1)?,
    ])?;
    let project = sum(&[
        metadata,
        next_prompt_contract_workspace()?.max(sum(&[
            MAX_MANIFEST_BYTES.max(
                agents::investigation::staged_finish::RefusalFeedback::max_bytes_with_policy(
                    policy,
                )
                .ok()?,
            ),
            slots::<Vec<u8>>(1)?,
            set::<&String>(attempts)?,
        ])?),
    ])?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let peak = sum(&[
        task,
        history,
        sweep
            .max(next_prompt_attempt_replay_workspace()?)
            .max(acquire)
            .max(coverage::archive_acquisition_workspace()?)
            .max(project),
    ])?;
    Some((history, project, peak))
}

/// Shared typed row owner for invocation and unvalidated refusal rows.
pub(super) fn row_workspace() -> Option<usize> {
    // Closed union of invocation/refusal rows. Every dynamic element consumes
    // an admitted JSON value; strings fit the row wire. No allocating defaults.
    // Refusals are not semantically validated by raw_refusals: use record caps,
    // not the smaller valid-feedback/assessment limits, even for rejected rows.
    let element = [
        size_of::<String>(),
        size_of::<AttemptRecord>(),
        size_of::<ObligationObservation>(),
        size_of::<FeedbackEntry>(),
        size_of::<DisclosedInvestigationFact>(),
        size_of::<DisclosedInvestigationQuery>(),
        size_of::<DisclosedInvestigationSource>(),
        size_of::<DisclosedInvestigationCoverageDetail>(),
    ]
    .into_iter()
    .max()?;
    sum(&[
        size_of::<PendingInvestigationStep>().max(size_of::<staged::JournalEntry>()),
        MAX_RECORD_BYTES,
        element.checked_mul(RECORD_MAX_VALUES)?,
    ])
}

/// Additional transient choices derivation; borrowed history/task are separate.
pub(super) fn acquisition_choices_workspace() -> Option<usize> {
    let policy = &INVESTIGATION_V2_POLICY;
    let attempts = policy.max_completed_attempts();
    let (ledger, decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    sum(&[
        map::<u64, InvestigationInputLedger>(attempts)?,
        ledger.checked_mul(attempts)?, // at most one owner per distinct revision
        decode,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?, // next named ledger read
        slots::<&AttemptRecord>(attempts)?,
        slots::<agents::investigation::staged_attempt::AttemptEvidence<'_>>(attempts)?,
        slots::<Vec<u8>>(3)?,
        next_prompt_metadata_workspace()?.1, // metadata_at remains live during derive
        next_prompt_contract_workspace()?,
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        set::<&str>(attempts)?,
        agents::investigation::acquisition_choices::AcquisitionChoices::candidate_workspace_bytes(
            policy,
        )?,
        // @21 retains the original candidate while scanning refusals and
        // constructing a filtered candidate; older versions use less workspace.
        admission_journal::refusal_workspace::capacity_exclusion_workspace()?,
        agents::investigation::acquisition_choices::AcquisitionChoices::candidate_workspace_bytes(
            policy,
        )?,
        json_bytes(MAX_ACTION_BYTES)?, // action Value used for canonical hash
        MAX_ACTION_BYTES,
        slots::<Vec<u8>>(1)?,               // rejected/final bounded encoding
        MAX_MANIFEST_BYTES.checked_mul(2)?, // canonical encoding/domain fingerprints
    ])
}

/// Metadata candidate snapshot and independent writer derivation. Borrowed
/// current/candidate inputs and transport owners are charged by the caller.
#[cfg(unix)]
pub fn next_prompt_metadata_continuation_workspace() -> Option<(usize, usize)> {
    let (history, metadata, load) = next_prompt_metadata_workspace()?;
    let (core, core_load) = next_prompt_saved_call_workspace()?;
    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let retained = sum(&[
        size_of::<metadata_admission::MetadataContinuationSnapshot>(),
        ledger, // previous ledger in the turn basis
        metadata,
        json_bytes(MAX_ACTION_BYTES)?, // final bounded choices retained by Arc
        agents::investigation::acquisition_choices::AcquisitionChoices::private_workspace_bytes(
            &INVESTIGATION_V2_POLICY,
        )?,
        64,
        agents::investigation::acquisition_choices::AuthenticatedCitationCorrection::retained_workspace_bytes(
            &INVESTIGATION_V2_POLICY,
        )?,
        128 + 6 + 20, // ledger hash and next action ID
    ])?;
    let stable = sum(&[
        task.checked_mul(2)?, // saved task plus projected usage candidate
        core,
        ledger, // borrowed candidate during validation/derivation
        metadata,
        row_workspace()?.checked_mul(INVESTIGATION_V2_POLICY.max_completed_attempts())?,
        retained,
    ])?;
    let peak = stable.checked_add(
        load.max(core_load)
            .max(next_prompt_correction_workspace()?)
            .max(history.checked_add(acquisition_choices_workspace()?)?),
    )?;
    Some((retained, peak))
}
