//! Read-only source continuation projection. Publication/commit is separate.
use super::*;
use context_hub::logical::{slots, sum};
use std::mem::size_of;

/// Retained returned snapshot and read peak. The normalized request, saved
/// authority and projected owners conservatively coexist through nested reads.
/// Caller source/guard/input owners are separate; no allocator/native/RSS claim.
pub fn workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let (task, task_load) = next_prompt_task_load_workspace()?;
    let (saved, saved_load) =
        read_reservations::workspace::next_prompt_read_reservation_workspace()?;
    let (core, core_load) = next_prompt_saved_call_workspace()?;
    let (loaded_ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    // The worker's acquired candidate appends one citation to its saved base.
    // Successful manifest encoding does not establish the decoder value count:
    // retain the extra typed citation plus strings copied from the admitted
    // target, new text hash and generated citation ID. All copied dynamic
    // fields occur in that target; its unused snapshot/target IDs overcount.
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
    let contract = next_prompt_contract_workspace()?;
    // Contract workspace includes normalized envelopes and their indexes;
    // keep that conservative currency for the returned normalized request.
    let request = sum(&[
        size_of::<StagedObligationRequest>(),
        MAX_QUESTION_BYTES,
        contract,
    ])?;
    let response = sum(&[size_of::<InvestigationResponseMeta>(), MAX_RECORD_BYTES])?;
    let n = policy.max_completed_attempts();
    let attempts = sum(&[
        slots::<Vec<completion::AttemptEntry>>(1)?,
        slots::<completion::AttemptEntry>(n)?,
        MAX_RECORD_BYTES.checked_mul(n)?,
    ])?;
    // materialize produces at most two events; identities come from admitted
    // task/step rows. Use full row payload for each before event encoding.
    let events = sum(&[
        slots::<Vec<InvestigationEvent>>(1)?,
        slots::<InvestigationEvent>(2)?,
        MAX_RECORD_BYTES.checked_mul(2)?,
    ])?;
    // Before final task encode, citations are cloned from the candidate and
    // completion observation can clone a raw refusal assessment. That raw row
    // has not passed the smaller semantic feedback limits. Original task,
    // candidate ledger and raw-row currencies cover these added dynamic fields.
    let projected_detail = sum(&[task, ledger, row])?;
    let projected = sum(&[
        size_of::<SettlementProjection>(),
        projected_detail,
        saved, // disposition is derived from the saved reservation
        events,
        attempts,
    ])?;
    // Successful encode bounds wire, not the strict decoder value count. Keep
    // the constructed detail currency even after materialize succeeds.
    // Returned snapshot: request, detail, base/candidate/previous ledgers,
    // metadata, turn hash and generated action ID (128-byte task ID + suffix).
    let retained = sum(&[
        size_of::<source_snapshot::SourceContinuationSnapshot>(),
        request,
        projected_detail,
        ledger.checked_mul(3)?,
        metadata,
        context_hub::logical::json_bytes(MAX_ACTION_BYTES)?,
        agents::investigation::acquisition_choices::AcquisitionChoices::private_workspace_bytes(
            policy,
        )?,
        64,
        agents::investigation::acquisition_choices::AuthenticatedCitationCorrection::retained_workspace_bytes(
            policy,
        )?,
        128 + 6 + 20,
    ])?;
    // derive_attempt_records creates a record from admitted task, source and
    // ledger fields before validating it. content_id clones that record before
    // bounded encoding. Accepted entries then have MAX_RECORD_BYTES payload.
    let constructing_attempt =
        sum(&[size_of::<AttemptRecord>(), task, saved, ledger, response])?.checked_mul(2)?;
    let attempt_validation = sum(&[
        constructing_attempt,
        attempt_binding_validation_workspace(policy, MAX_RECORD_BYTES)?,
        contract,
    ])?;
    // project_observation reloads attempts and at most two raw refusal rows.
    // The metadata-load component includes their typed rows and decoder scratch;
    // it is deliberately retained in full rather than treating it as return-only.
    let observation = sum(&[
        next_prompt_attempt_replay_workspace()?,
        metadata_load,
        projected,
        contract,
    ])?;
    // read_body retains wire during strict decode and canonical comparison.
    // Snapshot's prior ledgers are individually loaded/validated; a TurnBasis
    // also clones the base ledger while fingerprinting/validation runs.
    let reads = sum(&[
        ledger_decode,
        record_decode_scratch_bytes()?,
        MAX_RECORD_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
        4096,
        128,
        slots::<String>(3)?,
    ])?;
    let validation = sum(&[
        ledger,
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        contract,
        128,
        slots::<String>(2)?,
    ])?;
    // logical_delta encodes projected and original task in one expression.
    // All its encoders are bounded writers, including rejected projections.
    let encoding = sum(&[MAX_RECORD_BYTES.checked_mul(2)?, slots::<Vec<u8>>(2)?])?;
    // Unavailable reads reconstruct the prior turn: two ledgers and the bounded
    // event-sequence index survive each event read and ordering validation.
    let turn = sum(&[
        ledger.checked_mul(2)?,
        next_prompt_ledger_workspace()?,
        reads,
        validation,
        slots::<Vec<i64>>(1)?,
        slots::<i64>(
            usize::try_from(policy.journal_events())
                .ok()?
                .checked_add(1)?,
        )?,
        size_of::<InvestigationEvent>(),
        MAX_RECORD_BYTES,
    ])?;
    let stable = sum(&[
        request,
        task,
        saved,
        core,
        response,
        projected,
        metadata,
        ledger.checked_mul(2)?, // borrowed candidate plus current validation copy
        attempts.checked_mul(2)?, // suffix + appended IDs, or reloaded history + records
        retained,               // conservative overlap through returned DTO construction
    ])?;
    let peak = stable.checked_add(
        storage::archive_schema_workspace()?
            .max(task_load)
            .max(saved_load)
            .max(next_prompt_pending_source_workspace()?)
            .max(core_load)
            .max(metadata_load)
            .max(correction_workspace()?)
            .max(attempt_validation)
            .max(observation)
            .max(reads)
            .max(validation)
            .max(encoding)
            .max(turn)
            .max(sum(&[
                next_prompt_metadata_workspace()?.0,
                metadata_history_workspace::acquisition_choices_workspace()?,
            ])?),
    )?;
    Some((retained, peak))
}

/// The typed-correction loader keeps the authenticated journal while loading
/// the selected refusal basis a second time. Compose here, outside the base
/// journal/history calculators, so this cannot recurse into its own estimate.
pub fn correction_workspace() -> Option<usize> {
    let (journal, journal_peak) = next_prompt_staged_workspace()?;
    let (basis, basis_peak) = result_replay_workspace::completion_basis_workspace()?;
    let policy = &INVESTIGATION_V2_POLICY;
    let authentication = sum(&[
        journal,
        basis,
        agents::investigation::acquisition_choices::AuthenticatedCitationCorrection::retained_workspace_bytes(policy)?,
        agents::investigation::acquisition_choices::AuthenticatedCitationCorrection::projection_workspace_bytes(policy)?,
    ])?;
    Some(
        journal_peak
            .max(journal.checked_add(basis_peak)?)
            .max(authentication),
    )
}
