//! Acquisition and replay owners in completion::attempts; not outer task replay or RSS.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Caller/record acquisition stays live throughout every sequential replay
/// phase. Saved-call metadata and outer result/task owners remain separate.
#[cfg(unix)]
pub fn next_prompt_attempt_replay_workspace() -> Option<usize> {
    let policy = &INVESTIGATION_V2_POLICY;
    // Binding validation admits the whole AttemptRecord before read_action_hash
    // builds its smaller action Value. All action fields come from that record.
    let action = sum(&[
        json_bytes(MAX_RECORD_BYTES)?,
        MAX_ACTION_BYTES,
        slots::<Vec<u8>>(1)?,
        64,
        slots::<String>(1)?,
    ])?;
    let step = sum(&[MAX_INVOCATION_BYTES, slots::<Vec<u8>>(1)?])?;
    // Acquisition retains both ledgers. Turn validation also compares its
    // retained basis hash with a separately returned fingerprint String.
    let turn = sum(&[
        ledger_validation_workspace(policy, MAX_RECORD_BYTES)?,
        size_of::<InvestigationTurnBasis>(),
        128,                 // basis hash plus comparison fingerprint payloads
        slots::<String>(1)?, // returned fingerprint; basis header is inline
    ])?;
    let (archive, _) = InvestigationCoverageArchive::decode_workspace_bytes()?;
    let (_, read) = InvestigationCoverageArchive::read_workspace_bytes()?;
    let unavailable = sum(&[
        super::coverage::archive_acquisition_workspace()?,
        archive,
        read,
        size_of::<InvestigationCoverageRead>(),
        MAX_RECORD_BYTES, // three request strings cloned from the admitted record
    ])?;
    next_prompt_attempt_acquisition_workspace()?.checked_add(
        next_prompt_contract_workspace()?
            .max(completion::attempt_schema_workspace()?)
            .max(attempt_binding_validation_workspace(
                policy,
                MAX_RECORD_BYTES,
            )?)
            .max(action)
            .max(step)
            .max(turn)
            .max(unavailable),
    )
}

pub fn next_prompt_attempt_acquisition_workspace() -> Option<usize> {
    // Closed DTO union: only the invocation disclosure has dynamic containers.
    // Other nested fields are inline, with string payload bounded by record wire.
    // Optional serde defaults are None/empty; no default creates owned payload.
    let root = [
        size_of::<completion::AttemptEntry>(),
        size_of::<PendingInvestigationStep>(),
        size_of::<InvestigationResponseMeta>(),
        size_of::<InvestigationEvent>(),
    ]
    .into_iter()
    .max()?;
    let element = [
        size_of::<String>(),
        size_of::<DisclosedInvestigationFact>(),
        size_of::<DisclosedInvestigationQuery>(),
        size_of::<DisclosedInvestigationSource>(),
        size_of::<DisclosedInvestigationCoverageDetail>(),
    ]
    .into_iter()
    .max()?;
    let record = sum(&[
        root,
        MAX_RECORD_BYTES,
        element.checked_mul(RECORD_MAX_VALUES)?,
    ])?;
    // Deepest buffered tagged path: AttemptEntry.record.read (ReadIntent)
    // -> TaskFactKey. AttemptOutcome is a sibling; disclosure fact and
    // representation tags are siblings too. Response/event DTOs have no tags.
    // Malformed nested JSON is one tree per typed layer, not one full copy per
    // JSON depth. Keep the independent strict preflight scratch below (#969).
    let content = json_bytes(MAX_RECORD_BYTES)?.checked_mul(2)?;
    let decoder = record.checked_add(record_decode_scratch_bytes()?.max(content))?;
    let (ledger, ledger_decode) = InvestigationInputLedger::decode_workspace_bytes()?;
    let (task, _) = next_prompt_record_workspace(MAX_RECORD_BYTES)?;
    let limit = INVESTIGATION_V2_POLICY.max_completed_attempts();
    let sql = [
        ("completion_attempts", "sequence"),
        ("investigation_inputs", "revision"),
        ("investigation_responses", "step_id"),
        ("investigation_invocations", "step_id"),
        ("investigation_events", "sequence"),
    ]
    .into_iter()
    .map(|(table, key)| storage::body_read_sql(table, key).len())
    .max()?;
    // Keep accumulated entries, current response/event and both invocation
    // reads, plus current/prior ledgers, throughout every sequential decode.
    // Counting the acquiring record again is conservative. #924 eliminates
    // the former additional prior-ledger clone during turn validation.
    sum(&[
        task,
        slots::<Vec<i64>>(1)?,
        slots::<i64>(limit.checked_add(1)?)?,
        slots::<Vec<completion::AttemptEntry>>(1)?,
        record.checked_mul(limit.checked_add(4)?)?,
        ledger.checked_mul(2)?,
        MAX_RECORD_BYTES,
        slots::<Vec<u8>>(1)?,
        sql,
        128,
        slots::<String>(3)?,
        decoder.max(ledger_decode),
    ])
}
