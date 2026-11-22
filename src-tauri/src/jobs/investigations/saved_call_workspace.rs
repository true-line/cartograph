//! Single saved-call acquisition/validation; timeline accumulation is separate.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Owned SavedCallCore and peak including borrowed task/history/input ledger.
/// Schema preflight, accumulated Timeline.calls and outer task replay are not
/// covered. These logical owners are neither allocator sizes nor process RSS.
pub fn next_prompt_saved_call_workspace() -> Option<(usize, usize)> {
    let (retained, peak) = next_prompt_raw_call_workspace()?;
    Some((
        retained,
        peak.max(next_prompt_capacity_replay_workspace()?.1),
    ))
}
// Lower timeline construction must never recurse through full journal replay.
pub fn next_prompt_raw_call_workspace() -> Option<(usize, usize)> {
    let row = super::metadata_history_workspace::row_workspace()?;
    let (ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (_, projection, history_peak) = next_prompt_metadata_workspace()?;
    // Projection includes the owned metadata plus conservative validation
    // scratch. Keep it in the return bound rather than subtracting assumptions.
    let retained = sum(&[
        size_of::<admission_contract::SavedCallCore>(),
        row,
        ledger,
        projection,
    ])?;
    let row_decode = sum(&[
        row,
        record_decode_scratch_bytes()?
            .max(json_bytes(MAX_RECORD_BYTES)?.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?),
        MAX_RECORD_BYTES,
        128,
        slots::<Vec<u8>>(1)?,
        slots::<String>(3)?,
        4096, // fixed SQL/key/hash owners for named reads
    ])?;
    let ledger_read = sum(&[
        ledger_decode,
        MAX_RECORD_BYTES,
        128,
        slots::<Vec<u8>>(1)?,
        slots::<String>(3)?,
        storage::body_read_sql("investigation_inputs", "revision").len(),
    ])?;
    // validate_core runs these validations/fingerprints sequentially. Summing
    // their scratch conservatively preserves rejected-row encoding too.
    let validation = sum(&[
        next_prompt_contract_workspace()?,
        metadata_history_workspace::acquisition_choices_workspace()?,
        agents::investigation::disclosure_validation_workspace(
            &INVESTIGATION_V2_POLICY,
            MAX_RECORD_BYTES,
        )?,
        projection,
        MAX_RECORD_BYTES.checked_mul(2)?,
        slots::<Vec<u8>>(2)?,
        256, // retained fingerprint strings and domain prefixes
    ])?;
    // Timeline validation clones step/ledger while history and input ledger
    // remain live. Standalone metadata reloads its named step and keeps that
    // owner through replay. Reserve both layouts throughout, conservatively.
    // Moving raw refusal rows into a selected vector also overlaps both arrays.
    let peak = sum(&[
        history_peak,
        retained.checked_mul(2)?,
        slots::<staged::JournalEntry>(2)?,
        slots::<Vec<staged::JournalEntry>>(1)?,
        row_decode
            .max(ledger_read)
            .max(coverage::archive_acquisition_workspace()?)
            .max(validation),
    ])?;
    Some((retained, peak))
}
