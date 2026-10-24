//! Pure disclosure validation owners; borrowed records and host replay are separate.
use super::*;
use context_hub::logical::{json_bytes, slots, sum};
use std::mem::size_of;

/// Additional logical workspace for InvestigationDisclosure::validate_with_policy.
/// The caller admits the typed ledger's compact wire before entry. This includes
/// rejected public coverage encoding, but not caller DTOs, allocator/native
/// memory, saved-call replay or a complete worker/RSS bound.
pub fn disclosure_validation_workspace(
    policy: &InvestigationPolicy,
    typed_row_bytes: usize,
) -> Option<usize> {
    if typed_row_bytes > MAX_RECORD_BYTES {
        return None;
    }
    let queries = usize::try_from(policy.limits().tool_actions).ok()?;
    let citations = policy
        .limits()
        .selected_facts
        .checked_add(policy.limits().evidence_items)?;
    // from_proof validates again, then constructs public JSON before its wire
    // cap. The returned two identity strings and hash remain live afterwards.
    let coverage = sum(&[
        json_bytes(typed_row_bytes)?,
        MAX_INPUT_BYTES,
        slots::<Vec<u8>>(1)?,
        size_of::<DisclosedInvestigationCoverage>(),
        typed_row_bytes,
        64,
    ])?;
    // References accumulate while each next reference validates the ledger.
    // Registry hashing retains them; conversion moves hashes into query_ids.
    // Count both vector layouts conservatively, including the returned history.
    let history = sum(&[
        slots::<QueryHistoryReference>(queries)?,
        slots::<String>(queries)?,
        queries.checked_mul(64)?,
        slots::<Vec<QueryHistoryReference>>(1)?,
        size_of::<DisclosedQueryHistory>(),
        192, // request hash, registry hash, nested ledger fingerprint
        slots::<String>(1)?,
        MAX_MANIFEST_BYTES.checked_mul(2)?,
        b"cartograph:query-history-registry:v1\0".len(),
        slots::<Vec<u8>>(2)?,
    ])?;
    sum(&[
        ledger_validation_workspace(policy, typed_row_bytes)?,
        coverage,
        history,
        slots::<Vec<&InvestigationCitation>>(1)?,
        slots::<&InvestigationCitation>(citations)?,
        MAX_DISCLOSURE_BYTES,
        slots::<Vec<u8>>(1)?,
    ])
}
