//! Current saved-call and pending-consent validation, not whole task loading.
use super::*;
use context_hub::logical::{slots, sum};
use std::mem::size_of;

/// Peaks for validate_saved and validate_pending_consent respectively. Includes
/// the borrowed task and schema workspace; no source/result replay or RSS claim.
pub fn next_prompt_saved_validation_workspace() -> Option<(usize, usize)> {
    let row = metadata_history_workspace::row_workspace()?;
    let (_, core_peak) = next_prompt_saved_call_workspace()?;
    let (_, capacity_peak) = next_prompt_capacity_replay_workspace()?;
    let schema = storage::archive_schema_workspace()?;
    // validate_saved drops the core ledger and metadata before derive_at, but
    // the named invocation step survives through charged-usage validation.
    let saved = schema.checked_add(core_peak.max(row.checked_add(capacity_peak)?))?;

    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (_, metadata, metadata_peak) = next_prompt_metadata_workspace()?;
    // The original ledger remains live while metadata is acquired. Both stay
    // live across from_pre_call_saved's sequential second-witness checks.
    let originals = sum(&[ledger, metadata])?;
    let nested = core_peak // dominates prior invocation/second ledger acquisition
        .max(coverage::archive_acquisition_workspace()?)
        .max(metadata_peak)
        .max(capacity_peak);
    // from_pre_call constructs a fresh basis, cloning admission and task IDs,
    // then validates/fingerprints against the retained original witnesses.
    // A whole admitted task owner covers the cloned strings/admission, even
    // before validate_core rejects inconsistent witnesses. Input task is still
    // separately included by the nested/pure validation phase below.
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let constructed = sum(&[
        size_of::<admission_contract::SavedCallBasis>(),
        task,
        256,
        slots::<String>(4)?,
    ])?;
    let validation = sum(&[
        task,
        constructed,
        size_of::<admission_journal::CapacityState>(),
        64,
        slots::<String>(1)?,
        core_peak, // includes disclosure, metadata and contract fingerprints
    ])?;
    let consent = schema.checked_add(
        storage::next_prompt_ledger_workspace()?
            .max(ledger.checked_add(metadata_peak)?)
            .max(originals.checked_add(nested.max(validation))?),
    )?;
    Some((saved, consent))
}
