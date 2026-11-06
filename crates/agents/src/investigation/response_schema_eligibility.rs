//! @20 transport eligibility. Host completion admission remains authoritative.
use super::*;

/// Apply only after the ordinary selector and unknown-anchor recovery. The
/// authenticated choices derive pending state from completed attempt records.
pub(in crate::investigation) fn select_eligible_acquisition(
    ordinary: Option<String>,
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
) -> Result<Option<String>, InvestigationError> {
    if mode == CompletionCallMode::FinishOnly
        || (!choices.attempts_complete() && choices.entries().is_empty())
    {
        // Preserve the no-action blocked/deferred fallback. This is not a
        // completed attempt or permission to accept an unsupported finish.
        return Ok(ordinary);
    }
    let mut schema = decode(&ordinary)?;
    let ready = choices.attempts_complete();
    schema["anyOf"]
        .as_array_mut()
        .ok_or(InvestigationError::InvalidInput)?
        .retain(|branch| match branch["$ref"].as_str() {
            Some("#/$defs/insufficient_wrapper") => ready,
            Some("#/$defs/supported_wrapper") => ready && !ledger.citations.is_empty(),
            _ => !(ready && choices.all_acquired()),
        });
    if ready && !ledger.citations.is_empty() {
        set_citations(
            &mut schema,
            ledger.citations.iter().map(|c| c.citation_id.as_str()),
        );
    }
    encode_schema(&schema)
}

/// Sizing only: retain every historical branch, including both finishes, and
/// account for all projected citation handles (including a prospective source
/// append). This dominates the corresponding eligibility-narrowed schema.
pub(in crate::investigation) fn reserve_eligible_acquisition(
    ordinary: Option<String>,
    mode: CompletionCallMode,
    prompt: &Value,
) -> Result<Option<String>, InvestigationError> {
    if mode == CompletionCallMode::FinishOnly || ordinary.as_deref() == Some(FINISH) {
        // The selector returns the exact compiled (formatted) FINISH bytes
        // for its blocked/deferred no-action fallback. Actual eligibility
        // preserves these bytes too; do not compact them during sizing.
        return Ok(ordinary);
    }
    let citations = prompt["citations"]
        .as_array()
        .ok_or(InvestigationError::InvalidInput)?;
    if citations.is_empty() {
        return Ok(ordinary);
    }
    if citations
        .iter()
        .any(|c| c["citation_id"].as_str().is_none())
    {
        return Err(InvestigationError::InvalidInput);
    }
    let mut schema = decode(&ordinary)?;
    let generic = schema["$defs"]["finding"]["properties"]["citation_ids"]["items"].take();
    set_citations(
        &mut schema,
        citations.iter().filter_map(|c| c["citation_id"].as_str()),
    );
    let exact = schema["$defs"]["finding"]["properties"]["citation_ids"]["items"].take();
    // A one-short-ID enum can be smaller than the unchanged fallback's
    // generic items. Retain both representations in this sizing-only union;
    // it dominates either actual form at every outer JSON escaping layer.
    schema["$defs"]["finding"]["properties"]["citation_ids"]["items"] =
        serde_json::json!({"anyOf": [generic, exact]});
    encode_schema(&schema)
}

fn set_citations<'a>(schema: &mut Value, ids: impl Iterator<Item = &'a str>) {
    schema["$defs"]["finding"]["properties"]["citation_ids"]["items"] =
        serde_json::json!({"enum": ids.collect::<Vec<_>>()});
}
fn decode(ordinary: &Option<String>) -> Result<Value, InvestigationError> {
    serde_json::from_str(
        ordinary
            .as_deref()
            .ok_or(InvestigationError::InvalidInput)?,
    )
    .map_err(|_| InvestigationError::InvalidInput)
}
fn encode_schema(schema: &Value) -> Result<Option<String>, InvestigationError> {
    String::from_utf8(bounded_bytes(schema, MAX_INPUT_BYTES)?)
        .map(Some)
        .map_err(|_| InvestigationError::InvalidInput)
}
