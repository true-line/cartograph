//! Retained admission timeline and replay owners; refusal rows remain separate.
use super::*;
use crate::jobs::investigations::attempt_record_workspace::next_prompt_attempt_acquisition_workspace;
use context_hub::logical::{json_bytes, map, set, slots, sum};
use std::mem::size_of;

/// Logical retained Timeline and replay peak, including borrowed task/history.
/// Excludes read_entries, outer task replay, schema preflight and process RSS.
pub fn next_prompt_timeline_workspace() -> Option<(usize, usize)> {
    let policy = &INVESTIGATION_V2_POLICY;
    let calls = usize::try_from(policy.limits().model_invocations).ok()?;
    let events = usize::try_from(policy.journal_events()).ok()?;
    let (core, call_peak) = super::super::saved_call_workspace::next_prompt_raw_call_workspace()?;
    // Basis clones task identity, step disclosure and event step_id; those
    // originate in separately byte-admitted rows. Fingerprint is returned text.
    let basis = sum(&[
        size_of::<SavedCallBasis>(),
        MAX_RECORD_BYTES, // task id and intent hash
        MAX_RECORD_BYTES, // admission stamp and completion metadata hash
        16 * 1024,        // event step id
        64,               // returned ledger fingerprint
    ])?;
    // History preflights invocation count; sequential ordinals bind each core
    // to a distinct saved step. Publications use the larger event bound since
    // rejected histories can fail the final input-count check only after replay.
    let retained = sum(&[
        size_of::<Timeline>(),
        map::<u32, CallWitness>(calls)?,
        set::<u32>(calls)?, // at most one semantic refusal event per invocation
        core.checked_add(basis)?.checked_mul(calls)?,
        slots::<Publication>(events)?,
    ])?;
    let (_, _, history_peak) = next_prompt_metadata_workspace()?;
    let (ledger, _) = InvestigationInputLedger::decode_workspace_bytes()?;
    // validate_publication_response builds a direct query Value and may keep
    // it while building a replay Value. All dynamic fields are subsets of a
    // canonically byte-admitted ledger; wrapper keys, hashes and decimal index
    // fit 1024 additional bytes. Source/inventory ReadIntent clones coexist.
    let action_wire = MAX_RECORD_BYTES.checked_add(1024)?;
    let publication = sum(&[
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        json_bytes(action_wire)?.checked_mul(2)?,
        size_of::<ReadIntent>(),
        MAX_RECORD_BYTES,
        size_of::<QueryHistoryReference>(),
        MAX_ACTION_BYTES,
        MAX_MANIFEST_BYTES.checked_mul(2)?, // reference encoding + domain frame
        slots::<Vec<u8>>(3)?,
        256, // reference domain and simultaneously retained hash payloads
        slots::<String>(3)?,
    ])?;
    // TurnBasis retains its own hash while validation returns a second hash.
    let turn = sum(&[
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        size_of::<InvestigationTurnBasis>(),
        128,
        slots::<String>(1)?,
    ])?;
    let peak = sum(&[
        retained,
        history_peak,           // history/task stay live through every later phase
        basis,                  // next call before insertion, including rejected calls
        ledger.checked_mul(2)?, // current and prior during tool publication
        size_of::<InvestigationEvent>(),
        16 * 1024,
        size_of::<InvestigationResponseMeta>(),
        4096,
        call_peak
            .max(next_prompt_attempt_acquisition_workspace()?)
            .max(publication)
            .max(turn),
    ])?;
    Some((retained, peak))
}
