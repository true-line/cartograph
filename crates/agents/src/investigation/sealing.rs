//! Seal only an actual firewall preparation of the validated transient input.
use super::*;
use crate::TaskFactKey;
use core_prov::content_hash;
use llm::CompletionPayload;
use llm::bounded::PreparedBoundedCompletion;
use serde_json::{Value, json};

const MAX_PROMPT_DEPTH: usize = 40;
const MAX_PROMPT_VALUES: usize = 16384;
const MAX_PROMPT_ARRAY_ITEMS: usize = 32768;

pub(super) fn seal(
    ledger: &InvestigationInputLedger,
    original: &CompletionPayload,
    prepared: &PreparedBoundedCompletion,
) -> Result<InvestigationDisclosure, InvestigationError> {
    let egress = &prepared.preview().egress;
    let before = parse_prompt(&original.prompt)?;
    let after = parse_prompt(&egress.payload.prompt)?;
    // Protocol/basis/citation identities must survive redaction exactly. Source
    // and graph property values can change, and are hashed as actually supplied.
    for field in [
        "schema_version",
        "policy",
        "input_ledger_hash",
        "graph_snapshot_id",
        "scope_snapshot_id",
        "citations",
    ] {
        if before.get(field).is_none() || before.get(field) != after.get(field) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    if (before["schema_version"] == 3 || before["schema_version"] == 4)
        && (before.get("turn_progress").is_none()
            || before.get("turn_progress") != after.get("turn_progress"))
    {
        return Err(InvestigationError::InvalidInput);
    }
    if before["schema_version"] == 4 {
        for field in [
            "inspection_requirements",
            "inspection_requirements_hash",
            "inspection_progress",
        ] {
            if before.get(field).is_none() || before.get(field) != after.get(field) {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    if before.get("omitted_sources") != after.get("omitted_sources") {
        return Err(InvestigationError::InvalidInput);
    }
    let raw_facts = array(&before["compact_context"], "facts")?;
    let facts = array(&after["compact_context"], "facts")?;
    let raw_options = array(&before["compact_context"], "evidence_options")?;
    let options = array(&after["compact_context"], "evidence_options")?;
    let queries = array(&after["compact_context"], "queries")?;
    if facts.len() != raw_facts.len()
        || options.len() != raw_options.len()
        || queries.len() != ledger.queries.len()
        || egress.payload.spans.len() != original.spans.len()
    {
        return Err(InvestigationError::InvalidInput);
    }
    for (raw, supplied) in raw_facts.iter().zip(facts) {
        if key(raw, "reference")? != key(supplied, "reference")? {
            return Err(InvestigationError::InvalidInput);
        }
    }
    for (raw, supplied) in raw_options.iter().zip(options) {
        if key(raw, "fact")? != key(supplied, "fact")? {
            return Err(InvestigationError::InvalidInput);
        }
    }
    let mut disclosed_facts = Vec::new();
    for selected in &ledger.selected_facts {
        let representations = facts
            .iter()
            .filter(|f| key(f, "reference").is_ok_and(|key| key == selected.fact))
            .collect::<Vec<_>>();
        let inventories = options
            .iter()
            .filter(|f| key(f, "fact").is_ok_and(|key| key == selected.fact))
            .collect::<Vec<_>>();
        if representations.is_empty() || inventories.is_empty() {
            return Err(InvestigationError::InvalidInput);
        }
        disclosed_facts.push(DisclosedInvestigationFact {
            fact: selected.fact.clone(),
            representation: supplied(
                &json!({"facts": representations, "evidence_options": inventories}),
            )?,
        });
    }
    let mut sources = Vec::new();
    for citation in ledger
        .citations
        .iter()
        .filter(|c| !matches!(c.origin, InvestigationEvidenceOrigin::GraphMetadata))
    {
        let Some(span) = egress
            .payload
            .spans
            .iter()
            .find(|s| s.id == citation.citation_id)
        else {
            let omission = array(&after, "omitted_sources")?
                .iter()
                .find(|o| {
                    o["handle"]["citation_id"].as_str() == Some(citation.citation_id.as_str())
                })
                .ok_or(InvestigationError::InvalidInput)?;
            let reason = serde_json::from_value(omission["reason"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            sources.push(DisclosedInvestigationSource {
                citation_id: citation.citation_id.clone(),
                representation: DisclosureRepresentation::Omitted { reason },
                disclosed_text_hash: None,
                redacted: false,
            });
            continue;
        };
        let text_hash = content_hash(span.text.as_bytes());
        sources.push(DisclosedInvestigationSource {
            citation_id: citation.citation_id.clone(),
            representation: supplied(span)?,
            redacted: citation.text_hash.as_deref() != Some(text_hash.as_str()),
            disclosed_text_hash: Some(text_hash),
        });
    }
    let disclosure = InvestigationDisclosure {
        inspection_requirements_hash: after
            .get("inspection_requirements_hash")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or(InvestigationError::InvalidInput)
            })
            .transpose()?,
        schema_version: if before["schema_version"] == 4 { 2 } else { 1 },
        policy: COMPACT_DISCLOSURE_POLICY.into(),
        input_ledger_hash: ledger.fingerprint()?,
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: egress.payload_hash.clone(),
        facts: disclosed_facts,
        queries: queries
            .iter()
            .enumerate()
            .map(|(query_index, query)| {
                Ok(DisclosedInvestigationQuery {
                    query_index,
                    representation: supplied(query)?,
                })
            })
            .collect::<Result<_, InvestigationError>>()?,
        sources,
        history: ledger
            .supplied_history_hash
            .as_ref()
            .map(|_| {
                let history = after
                    .get("prior_findings_as_t3_history")
                    .filter(|history| !history.is_null())
                    .ok_or(InvestigationError::InvalidInput)?;
                supplied(history)
            })
            .transpose()?,
    };
    disclosure.validate(ledger)?;
    Ok(disclosure)
}

fn parse_prompt(raw: &str) -> Result<Value, InvestigationError> {
    strict::preflight(
        raw,
        MAX_INPUT_BYTES,
        MAX_PROMPT_DEPTH,
        MAX_PROMPT_VALUES,
        MAX_PROMPT_ARRAY_ITEMS,
    )?;
    serde_json::from_str(raw).map_err(|_| InvestigationError::InvalidInput)
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, InvestigationError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or(InvestigationError::InvalidInput)
}

fn key(value: &Value, field: &str) -> Result<TaskFactKey, InvestigationError> {
    serde_json::from_value(
        value
            .get(field)
            .cloned()
            .ok_or(InvestigationError::InvalidInput)?,
    )
    .map_err(|_| InvestigationError::InvalidInput)
}

fn supplied(value: &impl serde::Serialize) -> Result<DisclosureRepresentation, InvestigationError> {
    Ok(DisclosureRepresentation::Supplied {
        representation_hash: content_hash(&bounded_bytes(value, MAX_INPUT_BYTES)?),
    })
}
