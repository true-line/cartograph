//! Representation invariance shared by actual schema9 sealing and future
//! reserve closure checks. This checks protection, not semantic/journal truth.
use super::*;

pub(in crate::investigation) fn admission_protected_fields(
    ledger: &InvestigationInputLedger,
    original: &CompletionPayload,
    redacted: &CompletionPayload,
    policy: &InvestigationPolicy,
) -> Result<(), InvestigationError> {
    let before = parse_prompt(&original.prompt)?;
    let after = parse_prompt(&redacted.prompt)?;
    response_schema::validate(ledger, original, &before, policy)?;
    if original.response_schema != redacted.response_schema {
        return Err(InvestigationError::InvalidInput);
    }
    admission_protected_values(&before, &after, &original.system, &redacted.system)
}

/// Representation-only projection check. The caller already selected this
/// schema through the real typed selector; this grants no sealing authority.
pub(in crate::investigation) fn selected_schema_protected_fields(
    original: &CompletionPayload,
    redacted: &CompletionPayload,
    selected: &str,
) -> Result<(), InvestigationError> {
    if original.response_schema.as_deref() != Some(selected)
        || original.response_schema != redacted.response_schema
    {
        return Err(InvestigationError::InvalidInput);
    }
    let before = parse_prompt(&original.prompt)?;
    let after = parse_prompt(&redacted.prompt)?;
    admission_protected_values(&before, &after, &original.system, &redacted.system)
}

// Actual sealing already owns both preflighted trees. Borrow them instead of
// reparsing and retaining another pair; source preview uses the wrapper above.
pub(super) fn admission_protected_values(
    before: &Value,
    after: &Value,
    original_system: &str,
    redacted_system: &str,
) -> Result<(), InvestigationError> {
    if before["schema_version"] != 9 || original_system != redacted_system {
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
        "withheld_sources",
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
    let prior = array(before, "coverage_details")?;
    let actual = array(after, "coverage_details")?;
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
