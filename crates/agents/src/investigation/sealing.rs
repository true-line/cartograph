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

/// `policy` is the one the host resolved from the exact saved task limits. The
/// supplied prompt must carry exactly those limits; nothing is inferred from it.
pub(super) fn seal_with_policy(
    ledger: &InvestigationInputLedger,
    original: &CompletionPayload,
    prepared: &PreparedBoundedCompletion,
    policy: &InvestigationPolicy,
    private_targets: Option<&[InvestigationInspectionTarget]>,
) -> Result<InvestigationDisclosure, InvestigationError> {
    seal_with_admission_policy(ledger, original, prepared, policy, private_targets, None)
}

pub(super) fn seal_with_admission_policy(
    ledger: &InvestigationInputLedger,
    original: &CompletionPayload,
    prepared: &PreparedBoundedCompletion,
    policy: &InvestigationPolicy,
    private_targets: Option<&[InvestigationInspectionTarget]>,
    admission: Option<DisclosedAdmissionState>,
) -> Result<InvestigationDisclosure, InvestigationError> {
    seal_admission_inner(
        ledger,
        original,
        prepared,
        policy,
        private_targets,
        admission,
        None,
    )
}
pub(super) fn seal_admission_inner(
    ledger: &InvestigationInputLedger,
    original: &CompletionPayload,
    prepared: &PreparedBoundedCompletion,
    policy: &InvestigationPolicy,
    private_targets: Option<&[InvestigationInspectionTarget]>,
    admission: Option<DisclosedAdmissionState>,
    acquisition: Option<&InvestigationInput>,
) -> Result<InvestigationDisclosure, InvestigationError> {
    let egress = &prepared.preview().egress;
    let before = parse_prompt(&original.prompt)?;
    let after = parse_prompt(&egress.payload.prompt)?;
    // Schema 7 is the exact non-v1 completion prompt: its inspection targets are
    // carried once, inside the completion contract. v1 never produces it, and a
    // non-v1 completion prompt never uses schema 6.
    let schema = before["schema_version"].as_u64();
    let legacy = policy.profile() == INVESTIGATION_V1_POLICY.profile();
    if (schema == Some(9)) != admission.is_some() {
        return Err(InvestigationError::InvalidInput);
    }
    if let Some(state) = &admission {
        if let Some(input) = acquisition {
            state.validate_saved_role_shape(input.frozen_specialist()?, policy)?;
            if state.acquisition.as_ref() != Some(&input.acquisition_stamp(original)?) {
                return Err(InvestigationError::InvalidInput);
            }
        } else {
            response_schema::validate(ledger, original, &before, policy)?;
        }
        if original.response_schema != egress.payload.response_schema {
            return Err(InvestigationError::InvalidInput);
        }
        protection::admission_protected_values(
            &before,
            &after,
            &original.system,
            &egress.payload.system,
        )?;
        let metadata: staged_refusal::ProtectedCompletionMetadata =
            serde_json::from_value(before["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if acquisition.is_some() {
            state.validate_mode_with_metadata(&metadata, policy)?;
        } else {
            state.validate_with_metadata(&metadata, policy)?;
        }
        let historical_role = [
            SpecialistId::DomainAnalystV10,
            SpecialistId::EvidenceAuditorV10,
            SpecialistId::DomainAnalystV11,
            SpecialistId::DomainAnalystV12,
            SpecialistId::DomainAnalystV13,
            SpecialistId::DomainAnalystV14,
            SpecialistId::DomainAnalystV15,
            SpecialistId::DomainAnalystV17,
            SpecialistId::EvidenceAuditorV11,
        ]
        .iter()
        .any(|role| original.system == role.prompt());
        if (acquisition.is_none() && !historical_role)
            || state.turn_progress_hash
                != content_hash(&bounded_bytes(&before["turn_progress"], MAX_INPUT_BYTES)?)
            || serde_json::to_value(&state.consumed_budgets)
                .map_err(|_| InvestigationError::InvalidInput)?
                != before["consumed_budgets"]
            || serde_json::to_value(state.mode).map_err(|_| InvestigationError::InvalidInput)?
                != before["turn_progress"]["admission_mode"]
            || Some(u64::from(state.capacity_refusal_count))
                != before["turn_progress"]["capacity_refusal_count"].as_u64()
            || serde_json::to_value(&state.capacity_feedback)
                .map_err(|_| InvestigationError::InvalidInput)?
                != before["turn_progress"]["capacity_feedback"]
        {
            return Err(InvestigationError::InvalidInput);
        }
    }
    if (matches!(schema, Some(7..=9)) && legacy) || (schema == Some(6) && !legacy) {
        return Err(InvestigationError::InvalidInput);
    }
    let limits =
        serde_json::to_value(policy.limits()).map_err(|_| InvestigationError::InvalidInput)?;
    if before.get("limits") != Some(&limits) || after.get("limits") != Some(&limits) {
        return Err(InvestigationError::InvalidInput);
    }
    if let Some(proof) = &ledger.coverage {
        let public = serde_json::to_value(&proof.qualification)
            .map_err(|_| InvestigationError::InvalidInput)?;
        if !matches!(schema, Some(5..=9))
            || before.get("coverage_qualification") != Some(&public)
            || before.get("context_id").and_then(Value::as_str) != Some(proof.context_id.as_str())
            || before.get("coverage_qualification") != after.get("coverage_qualification")
            || before.get("context_id") != after.get("context_id")
        {
            return Err(InvestigationError::InvalidInput);
        }
    } else if matches!(schema, Some(5..=9)) {
        return Err(InvestigationError::InvalidInput);
    }
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
    if matches!(schema, Some(3..=9))
        && (before.get("turn_progress").is_none()
            || before.get("turn_progress") != after.get("turn_progress"))
    {
        return Err(InvestigationError::InvalidInput);
    }
    if matches!(schema, Some(4..=6)) {
        for field in [
            "inspection_requirements",
            "inspection_requirements_hash",
            "inspection_progress",
        ] {
            if (before["schema_version"] == 4 && before.get(field).is_none())
                || before.get(field) != after.get(field)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    if matches!(schema, Some(7..=9)) {
        if before.get("inspection_requirements").is_some()
            || after.get("inspection_requirements").is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        for field in ["inspection_requirements_hash", "inspection_progress"] {
            if before.get(field) != after.get(field) {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    let (completion_contract_hash, completion_metadata_hash) = if matches!(schema, Some(6..=9)) {
        for field in ["completion_contract", "completion_metadata"] {
            if before.get(field).is_none() || before.get(field) != after.get(field) {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let contract: InvestigationCompletionContract =
            serde_json::from_value(before["completion_contract"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let metadata: staged_refusal::ProtectedCompletionMetadata =
            serde_json::from_value(before["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if contract.request_hash != metadata.request_hash
            || contract.obligations_hash != metadata.obligations_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        let hashes = (
            Some(contract.fingerprint_with_policy(policy)?),
            Some(metadata.fingerprint_with_policy(policy)?),
        );
        if matches!(schema, Some(7..=9)) {
            contract_targets(&before, &contract, policy, private_targets)?;
        }
        hashes
    } else {
        if before.get("completion_contract").is_some()
            || before.get("completion_metadata").is_some()
            || after.get("completion_contract").is_some()
            || after.get("completion_metadata").is_some()
        {
            return Err(InvestigationError::InvalidInput);
        }
        (None, None)
    };
    if before.get("omitted_sources") != after.get("omitted_sources")
        || before.get("withheld_sources") != after.get("withheld_sources")
    {
        return Err(InvestigationError::InvalidInput);
    }
    let raw_facts = array(&before["compact_context"], "facts")?;
    let facts = array(&after["compact_context"], "facts")?;
    let raw_options = array(&before["compact_context"], "evidence_options")?;
    let options = array(&after["compact_context"], "evidence_options")?;
    let progressive = matches!(schema, Some(8 | 9));
    let (query_history, progressive_queries) = if progressive {
        if before["policy"] != QUERY_HISTORY_DISCLOSURE_POLICY {
            return Err(InvestigationError::InvalidInput);
        }
        let original_context: ProgressiveInvestigationContext =
            serde_json::from_value(before["compact_context"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let actual_context: ProgressiveInvestigationContext =
            serde_json::from_value(after["compact_context"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let request_hash = before["completion_contract"]["request_hash"]
            .as_str()
            .ok_or(InvestigationError::InvalidInput)?;
        original_context
            .query_history
            .validate_registry(request_hash, ledger, policy)?;
        if original_context.query_history != actual_context.query_history {
            return Err(InvestigationError::InvalidInput);
        }
        let mut queries = original_context
            .query_history
            .prior_queries
            .iter()
            .map(|reference| DisclosedInvestigationQuery {
                query_index: reference.query_index,
                representation: DisclosureRepresentation::Omitted {
                    reason: DisclosureOmissionReason::ReferenceOnly,
                },
            })
            .collect::<Vec<_>>();
        if let Some(latest) = &actual_context.query_history.latest_query {
            // Repeated acquisitions can share interned objects. The ledger's
            // total returned-fact count is only a loose upper bound; a latest
            // view must index representations actually supplied in this payload.
            if latest
                .context
                .fact_indexes
                .iter()
                .any(|index| *index >= facts.len())
                || latest
                    .evidence_option_indexes
                    .iter()
                    .any(|index| *index >= options.len())
            {
                return Err(InvestigationError::InvalidInput);
            }
            queries.push(DisclosedInvestigationQuery {
                query_index: latest.reference.query_index,
                representation: supplied(latest)?,
            });
        }
        (
            Some(DisclosedQueryHistory::from_ledger(
                request_hash,
                ledger,
                policy,
            )?),
            queries,
        )
    } else {
        (None, Vec::new())
    };
    let queries: &[Value] = if progressive {
        &[]
    } else {
        array(&after["compact_context"], "queries")?
    };
    if facts.len() != raw_facts.len()
        || options.len() != raw_options.len()
        || (!progressive && queries.len() != ledger.queries.len())
        || egress.payload.spans.len() != original.spans.len()
    {
        return Err(InvestigationError::InvalidInput);
    }
    if ledger.coverage.is_some() && !progressive {
        let raw_queries = array(&before["compact_context"], "queries")?;
        for (original, supplied) in raw_queries.iter().zip(queries) {
            if original.get("qualification").is_none()
                || original.get("qualification") != supplied.get("qualification")
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
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
            if withheld_source_ids(&after)?.contains(citation.citation_id.as_str()) {
                sources.push(DisclosedInvestigationSource {
                    citation_id: citation.citation_id.clone(),
                    representation: DisclosureRepresentation::FirewallRefused,
                    disclosed_text_hash: None,
                    redacted: false,
                });
                continue;
            }
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
    let mut coverage_details = Vec::new();
    if ledger.coverage.is_some() {
        let originals = array(&before, "coverage_details")?;
        let actuals = array(&after, "coverage_details")?;
        if originals.len() != ledger.coverage_reads.len() || actuals.len() != originals.len() {
            return Err(InvestigationError::InvalidInput);
        }
        for (index, ((original, actual), read)) in originals
            .iter()
            .zip(actuals)
            .zip(&ledger.coverage_reads)
            .enumerate()
        {
            let original: InvestigationCoverageDetail = serde_json::from_value(original.clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            let actual_detail: InvestigationCoverageDetail = serde_json::from_value(actual.clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            if original.schema_version != 1
                || original.context_id != read.request.context_id
                || original.repo != read.request.repo
                || original.inventory_hash != read.request.inventory_hash
                || content_hash(original.inventory_json.as_bytes()) != read.request.inventory_hash
                || actual_detail.schema_version != original.schema_version
                || actual_detail.context_id != original.context_id
                || actual_detail.repo != original.repo
                || actual_detail.inventory_hash != original.inventory_hash
            {
                return Err(InvestigationError::InvalidInput);
            }
            let disclosed_body_hash = content_hash(actual_detail.inventory_json.as_bytes());
            coverage_details.push(DisclosedInvestigationCoverageDetail {
                read_index: index,
                representation_hash: content_hash(&bounded_bytes(actual, MAX_INPUT_BYTES)?),
                redacted: disclosed_body_hash != read.request.inventory_hash,
                disclosed_body_hash,
            });
        }
    }
    let mut artifact_pages = Vec::new();
    if !ledger.artifact_reads.is_empty() {
        let originals = array(&before, "artifact_pages")?;
        let actuals = array(&after, "artifact_pages")?;
        if originals.len() != ledger.artifact_reads.len() || actuals.len() != originals.len() {
            return Err(InvestigationError::InvalidInput);
        }
        for (index, ((original, actual), read)) in originals
            .iter()
            .zip(actuals)
            .zip(&ledger.artifact_reads)
            .enumerate()
        {
            let original: InvestigationArtifactPage = serde_json::from_value(original.clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            let actual_page: InvestigationArtifactPage = serde_json::from_value(actual.clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
            // Only the content may be redacted; every identity field is kept.
            if content_hash(&bounded_bytes(&original, MAX_INPUT_BYTES)?) != read.response_hash
                || actual_page.schema_version != original.schema_version
                || actual_page.context_id != original.context_id
                || actual_page.artifact_id != original.artifact_id
                || actual_page.schema != original.schema
                || actual_page.content_id != original.content_id
                || actual_page.offset != original.offset
                || actual_page.next_offset != original.next_offset
                || actual_page.total_content_bytes != original.total_content_bytes
                || actual_page.complete_system != original.complete_system
            {
                return Err(InvestigationError::InvalidInput);
            }
            let disclosed_body_hash = content_hash(&bounded_bytes(&actual_page, MAX_INPUT_BYTES)?);
            artifact_pages.push(DisclosedInvestigationArtifactPage {
                read_index: index,
                representation_hash: content_hash(&bounded_bytes(actual, MAX_INPUT_BYTES)?),
                redacted: disclosed_body_hash != read.response_hash,
                disclosed_body_hash,
            });
        }
    }
    let disclosure = InvestigationDisclosure {
        artifact_pages,
        admission,
        query_history,
        completion_contract_hash,
        completion_metadata_hash,
        coverage_details,
        coverage: ledger
            .coverage
            .as_ref()
            .map(DisclosedInvestigationCoverage::from_proof)
            .transpose()?,
        inspection_requirements_hash: after
            .get("inspection_requirements_hash")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or(InvestigationError::InvalidInput)
            })
            .transpose()?,
        schema_version: if schema == Some(9) {
            6
        } else if progressive {
            5
        } else if matches!(schema, Some(6 | 7)) {
            4
        } else if before["schema_version"] == 5 {
            3
        } else if before["schema_version"] == 4 {
            2
        } else {
            1
        },
        policy: if progressive {
            QUERY_HISTORY_DISCLOSURE_POLICY
        } else {
            COMPACT_DISCLOSURE_POLICY
        }
        .into(),
        input_ledger_hash: ledger.fingerprint_with_policy(policy)?,
        graph_snapshot_id: ledger.graph_snapshot_id.clone(),
        scope_snapshot_id: ledger.scope_snapshot_id.clone(),
        payload_hash: egress.payload_hash.clone(),
        facts: disclosed_facts,
        queries: if progressive {
            progressive_queries
        } else {
            queries
                .iter()
                .enumerate()
                .map(|(query_index, query)| {
                    Ok(DisclosedInvestigationQuery {
                        query_index,
                        representation: supplied(query)?,
                    })
                })
                .collect::<Result<_, InvestigationError>>()?
        },
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
    if let Some(input) = acquisition {
        disclosure.validate_saved_role_shape(ledger, policy, input.frozen_specialist()?)?;
    } else {
        disclosure.validate_with_policy(ledger, policy)?;
    }
    Ok(disclosure)
}

/// Reconstruct the canonical inspection targets from the validated contract and
/// bind them to the supplied hash, the supplied progress order and the host's
/// private admitted targets. The prompt holds no second descriptor copy.
fn contract_targets(
    before: &Value,
    contract: &InvestigationCompletionContract,
    policy: &InvestigationPolicy,
    private_targets: Option<&[InvestigationInspectionTarget]>,
) -> Result<(), InvestigationError> {
    let sources = contract.obligations.sources_with_policy(policy)?;
    let targets = if sources.is_empty() {
        None
    } else {
        Some(normalize_inspection_targets_with_policy(&sources, policy)?)
    };
    if targets.as_deref() != private_targets {
        return Err(InvestigationError::InvalidInput);
    }
    let Some(targets) = targets else {
        return if before.get("inspection_requirements_hash").is_some()
            || before.get("inspection_progress").is_some()
        {
            Err(InvestigationError::InvalidInput)
        } else {
            Ok(())
        };
    };
    let hash = inspection_requirements_hash_with_policy(&targets, policy)?;
    let progress = before
        .get("inspection_progress")
        .and_then(|progress| progress.get("requirements"))
        .and_then(Value::as_array)
        .ok_or(InvestigationError::InvalidInput)?;
    if before
        .get("inspection_requirements_hash")
        .and_then(Value::as_str)
        != Some(hash.as_str())
        || progress.len() != targets.len()
        || progress.iter().zip(&targets).any(|(entry, target)| {
            entry.get("target_id").and_then(Value::as_str) != Some(target.target_id.as_str())
        })
    {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
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

#[cfg(test)]
#[path = "query_history_sealing_tests.rs"]
mod query_history_tests;

#[path = "sealing_protection.rs"]
mod protection;
pub(super) use protection::{admission_protected_fields, selected_schema_protected_fields};
