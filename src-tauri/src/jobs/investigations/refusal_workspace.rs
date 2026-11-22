//! Admission refusal replay after timeline construction; not outer task replay.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Logical returned entries and complete derive_at/derive_current peak. Schema
/// validation is included; append, outer task replay and RSS are not covered.
pub fn next_prompt_capacity_replay_workspace() -> Option<(usize, usize)> {
    let calls = usize::try_from(INVESTIGATION_V2_POLICY.limits().model_invocations).ok()?;
    // Entry has a closed scalar/string layout plus its arbitrary proposal Value.
    // read_body admits canonical record bytes before the smaller entry checks.
    let row = sum(&[
        size_of::<Entry>(),
        MAX_RECORD_BYTES,
        json_bytes(MAX_RECORD_BYTES)?,
    ])?;
    let retained = sum(&[slots::<Vec<Entry>>(1)?, row.checked_mul(calls)?])?;
    let keys = sum(&[
        slots::<Vec<(u32, String, u32)>>(1)?,
        slots::<(u32, String, u32)>(calls.checked_add(1)?)?,
        calls.checked_add(1)?.checked_mul(128)?,
    ])?;
    let projection = sum(&[
        slots::<Vec<&Entry>>(1)?,
        slots::<&Entry>(calls)?,
        size_of::<CapacityState>(),
        64,  // capacity feedback action hash
        128, // semantic feedback action + choices hashes
        64,
        slots::<String>(1)?, // writer's expected canonical feedback hash
    ])?;
    let acquisition = sum(&[
        record_decode_scratch_bytes()?,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        slots::<String>(2)?,
        4096, // fixed read_body SQL plus the admitted 64-byte body hash
    ])?;
    let identity = sum(&[
        row, // content_id retains its clone alongside the original
        MAX_ENTRY_BYTES.checked_mul(2)?,
        b"cartograph:investigation-admission-refusal:v1\0".len(),
        slots::<Vec<u8>>(2)?,
        64,
        slots::<String>(1)?,
    ])?;
    let (timeline, timeline_peak) = next_prompt_timeline_workspace()?;
    let replay = sum(&[
        next_prompt_record_workspace(MAX_RECORD_BYTES)?.0, // caller task remains live
        timeline,
        next_prompt_metadata_workspace()?.0, // shared history survives read_entries
        retained,
        keys,
        row, // acquiring/validating next entry before pushing it
        size_of::<InvestigationResponseMeta>(),
        4096,
        projection, // historical feedback lives through current entry validation
        storage::archive_schema_workspace()?,
        64,
        slots::<String>(1)?, // cloned previous_entry_id
        acquisition
            .max(identity)
            .max(
                proposal_workspace()?
                    .checked_add(metadata_history_workspace::acquisition_choices_workspace()?)?,
            )
            .max(next_prompt_contract_workspace()?) // includes 256-byte response redaction
            .max(MAX_INVOCATION_BYTES.checked_add(slots::<Vec<u8>>(1)?)?),
    ])?;
    Some((retained, timeline_peak.max(replay)))
}

fn proposal_workspace() -> Option<usize> {
    // validate_proposal excludes finish/schema-version envelopes before decode.
    // The source-read chain can retain progressive, completion, qualified and
    // legacy original Values plus legacy's comparison. Tagged read DTO parsing
    // may buffer content at each admitted nesting level before semantic checks.
    let object = json_bytes(MAX_RECORD_BYTES)?;
    sum(&[
        object.checked_mul(5)?,
        object.checked_mul(RECORD_MAX_DEPTH.checked_add(1)?)?,
        object.checked_mul(2)?, // typed read fields and replay selector clone
        slots::<CompletionInvestigationAction>(2)?,
        MAX_ACTION_BYTES.checked_mul(2)?, // proposal encoding and parser scratch
        record_decode_scratch_bytes()?,
        ledger_validation_workspace(&INVESTIGATION_V2_POLICY, MAX_RECORD_BYTES)?,
        MAX_MANIFEST_BYTES.checked_mul(2)?, // reference encoding and domain frame
        b"cartograph:query-history-entry:v1\0".len(),
        slots::<Vec<u8>>(3)?,
        128,
        slots::<String>(2)?,
    ])
}

/// Prefix reconstruction used by choices derivation. Does not recurse through
/// timeline/history replay; borrowed task, history and original choices belong
/// to the caller. Hash strings are validated before entering the retained set.
pub(in crate::jobs::investigations) fn capacity_exclusion_workspace() -> Option<usize> {
    let calls = usize::try_from(INVESTIGATION_V2_POLICY.limits().model_invocations).ok()?;
    let row = sum(&[
        size_of::<Entry>(),
        MAX_RECORD_BYTES,
        json_bytes(MAX_RECORD_BYTES)?,
    ])?;
    let acquisition = sum(&[
        record_decode_scratch_bytes()?,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        slots::<String>(2)?,
        4096,
    ])?;
    let identity = sum(&[
        row,
        MAX_ENTRY_BYTES.checked_mul(2)?,
        b"cartograph:investigation-admission-refusal:v1\0".len(),
        slots::<Vec<u8>>(2)?,
        64,
        slots::<String>(1)?,
    ])?;
    sum(&[
        storage::archive_schema_workspace()?,
        slots::<Vec<u32>>(1)?,
        slots::<u32>(calls.checked_add(1)?)?,
        context_hub::logical::set::<String>(calls)?,
        calls.checked_mul(64)?,
        row,
        acquisition.max(identity).max(sum(&[
            proposal_workspace()?,
            json_bytes(MAX_ACTION_BYTES)?, // normalized query Value and encoding
            MAX_ACTION_BYTES,
        ])?),
    ])
}
