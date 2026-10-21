//! Representation invariance shared by actual schema9 sealing and future
//! reserve closure checks. This checks protection, not semantic/journal truth.
use super::*;

pub(in crate::investigation) fn admission_protected_fields(
    original: &CompletionPayload,
    redacted: &CompletionPayload,
) -> Result<(), InvestigationError> {
    let before = parse_prompt(&original.prompt)?;
    let after = parse_prompt(&redacted.prompt)?;
    if before["schema_version"] != 9 || original.system != redacted.system {
        return Err(InvestigationError::InvalidInput);
    }
    for name in [
        "schema_version",
        "policy",
        "limits",
        "input_ledger_hash",
        "graph_snapshot_id",
        "scope_snapshot_id",
        "context_id",
        "coverage_qualification",
        "citations",
        "completion_contract",
        "completion_metadata",
        "consumed_budgets",
        "turn_progress",
    ] {
        if before.get(name).is_none() || before.get(name) != after.get(name) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    for name in [
        "inspection_requirements_hash",
        "inspection_progress",
        "omitted_sources",
    ] {
        if before.get(name) != after.get(name) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    if before["compact_context"].get("query_history").is_none()
        || before["compact_context"].get("query_history")
            != after["compact_context"].get("query_history")
    {
        return Err(InvestigationError::InvalidInput);
    }
    let prior = array(&before, "coverage_details")?;
    let actual = array(&after, "coverage_details")?;
    if prior.len() != actual.len() {
        return Err(InvestigationError::InvalidInput);
    }
    for (prior, actual) in prior.iter().zip(actual) {
        for key in ["schema_version", "context_id", "repo", "inventory_hash"] {
            if prior.get(key).is_none() || prior.get(key) != actual.get(key) {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    Ok(())
}
