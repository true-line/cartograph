//! Immutable source reservation loading and pending validation; not source I/O.
use super::*;
use context_hub::logical::{json_bytes, map, slots, sum};
use std::mem::size_of;

/// Retained SavedRead and complete compatible load peak. Row bodies are capped
/// at 131072 by read/preflight, before serde or smaller semantic validation.
pub fn next_prompt_read_reservation_workspace() -> Option<(usize, usize)> {
    let wire = 131_072usize;
    if MAX_MANIFEST_BYTES != wire {
        return None;
    }
    // These closed records own strings/scalars, not unbounded typed vectors.
    // All three coexist even when an orphan or inconsistent row is rejected.
    let retained = sum(&[size_of::<SavedRead>(), wire.checked_mul(3)?])?;
    let decoder = sum(&[
        wire.checked_mul(2)?, // row and serde string scratch
        // ReadIntent::Source -> TaskFactKey is the deepest tagged path (2).
        // source.fact has only TaskFactKey; other fields are closed structs,
        // strings or scalars. Malformed nested input occupies one buffered
        // Content tree per typed layer, not one whole tree per JSON depth.
        json_bytes(wire)?.checked_mul(2)?,
        wire.checked_mul(6)?, // canonical escaping before encode rejects
        slots::<Vec<u8>>(3)?,
        64,
        slots::<String>(2)?,
        4096, // fixed reader SQL
    ])?;
    let preflight = sum(&[
        storage::archive_schema_workspace()?,
        map::<String, usize>(MAX_TASKS)?,
        MAX_TASKS.checked_mul(128)?,
        128, // next key before insertion
        slots::<String>(2)?,
        4096,
    ])?;
    // project_accounting validates reservation, charge and disposition.
    // Fingerprints may retain constructed expected records and a cloned
    // inspection target while serializing source/read/reservation identities.
    let validate = sum(&[
        retained,
        retained, // expected charge/disposition and target clone (conservative)
        wire.checked_mul(6)?.checked_mul(3)?, // encoding + domain + nested encoding
        slots::<Vec<u8>>(3)?,
        512,
        slots::<String>(8)?,
    ])?;
    Some((
        retained,
        preflight.max(retained.checked_add(decoder)?).max(validate),
    ))
}

/// Pending source authority replay. Borrowed task, saved reservation, second
/// call core, response and hashes survive their nested validation phases.
/// Excludes source acquisition, publication, outer result loading and RSS.
pub fn next_prompt_pending_source_workspace() -> Option<usize> {
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let (saved, load) = next_prompt_read_reservation_workspace()?;
    let (core, core_peak) = next_prompt_saved_call_workspace()?;
    let current_call = next_prompt_saved_validation_workspace()?.0;
    let response = sum(&[size_of::<InvestigationResponseMeta>(), MAX_RECORD_BYTES])?;
    let acquisition = sum(&[
        response,
        record_decode_scratch_bytes()?,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        128,
        slots::<String>(3)?,
        4096,
    ])?;
    let checks = sum(&[
        response,
        // Includes metadata fingerprint, response redaction and step/usage
        // validation; source/charge identities are also rechecked below.
        next_prompt_contract_workspace()?,
        load,
        4096usize.checked_mul(2)?, // owner identity encoding and domain frame
        slots::<Vec<u8>>(2)?,
        slots::<InvestigationUsage>(2)?,
        256,
        slots::<String>(4)?,
    ])?;
    let later = sum(&[
        task,
        saved,
        core,
        64,
        slots::<String>(1)?, // retained archive hash
        acquisition.max(checks),
    ])?;
    Some(
        task.checked_add(load)?
            .max(saved.checked_add(current_call)?)
            .max(saved.checked_add(core_peak)?)
            .max(later),
    )
}

/// CapacitySnapshot return currency and its derivation peak. Preflight proves
/// at most MAX_TASKS distinct 128-byte keys, including charges/dispositions via
/// their reservation foreign keys. Historical rows are replayed one at a time.
/// Borrowed caller task/projection and native SQLite/allocator/RSS are separate.
pub fn next_prompt_source_capacity_workspace() -> Option<(usize, usize)> {
    let retained = sum(&[
        size_of::<CapacitySnapshot>(),
        map::<String, CapacityCharge>(MAX_TASKS)?,
        MAX_TASKS.checked_mul(128)?,
    ])?;
    let pending = sum(&[
        size_of::<BTreeSet<String>>(),
        map::<String, ()>(MAX_TASKS)?,
        MAX_TASKS.checked_mul(128)?,
    ])?;
    // derive_capacity retains the accounting map and pending set during
    // load_validated. SavedRead includes every charge/disposition plus the
    // malformed-row decoder and identity-validation construction peaks.
    let replay = next_prompt_read_reservation_workspace()?.1;
    let peak = sum(&[
        retained,
        pending,
        size_of::<ReadTransaction<'_>>(),
        128usize.checked_mul(4)?, // task, step, pending insertion and map clone
        slots::<String>(4)?,
        4096usize.checked_mul(2)?, // outer scan and nested reader SQL
        replay,
    ])?;
    Some((retained, peak))
}
