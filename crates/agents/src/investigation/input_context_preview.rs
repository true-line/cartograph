//! Exact, deterministic metadata previews, before acquisition publication.
//!
//! A preview is not source access, tool admission, database provenance, a seal,
//! or dispatch authority. The host must retain its prepared metadata and bind
//! publication to this exact base/candidate on the authoritative transaction.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
use super::*;
use llm::bounded::{BoundedWireMeasurements, CompletionLimits, ProviderProfile};
use serde::Serialize;
use staged_refusal::{CompletionCallMode, ProtectedCompletionMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPreviewKind {
    Query,
    Inventory,
    /// A compiled-artifact page (ADR-0089).
    Artifact,
}
impl ContextPreviewKind {
    fn tool(self) -> InvestigationTool {
        match self {
            Self::Query => InvestigationTool::QueryContext,
            Self::Inventory => InvestigationTool::ReadCoverageInventory,
            Self::Artifact => InvestigationTool::ReadArtifact,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPreviewMeasurements {
    pub current: BoundedWireMeasurements,
    pub candidate: BoundedWireMeasurements,
    pub continuation: AdmissionReserveMeasurements,
}

/// Opaque, in-process size/basis proof; it cannot be deserialized, converted to
/// an input, or used as authorization. Changed raw bytes, request/metadata,
/// ledgers, usage, profile, limits or immediate action ID require a new proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPreviewBound {
    basis_hash: String,
    measurements: ContextPreviewMeasurements,
}
impl ContextPreviewBound {
    pub fn basis_hash(&self) -> &str {
        &self.basis_hash
    }
    pub fn measurements(&self) -> ContextPreviewMeasurements {
        self.measurements
    }
    /// Recompute against the exact prepared publication, rather than accepting
    /// a proof for another equally sized candidate. Host ownership/revision,
    /// raw metadata provenance and prospective attempt derivation remain host
    /// checks. Actual protected-field sealing must independently succeed.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_with_policy(
        &self,
        current: &InvestigationInput,
        candidate: &InvestigationInput,
        kind: ContextPreviewKind,
        raw_response: &str,
        admitted_usage: &InvestigationUsage,
        profile: &ProviderProfile,
        limits: &CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if *self
            != current.certify_context_preview_v9_with_policy(
                candidate,
                kind,
                raw_response,
                admitted_usage,
                profile,
                limits,
                action_id,
                policy,
            )?
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

impl InvestigationInput {
    /// Certify a candidate built by the actual prompt9 builder from one exact
    /// retained query/inventory response. The full outer UTF-8 response, not its
    /// inner inventory/facts, must match the newly appended manifest and cap.
    /// `admitted_usage` is the host's exact saved post-response/pre-tool usage;
    /// this pure method checks its arithmetic, not provider-reported provenance.
    /// Prospective attempt IDs must already be derived by the host's shared
    /// publication kernel. Future completion/capacity feedback and attempt slots
    /// are independently reserved using the production enclosing serializer.
    /// This is not a bound on an unprepared future query or unread source.
    #[allow(clippy::too_many_arguments)]
    pub fn certify_context_preview_v9_with_policy(
        &self,
        candidate: &Self,
        kind: ContextPreviewKind,
        raw_response: &str,
        admitted_usage: &InvestigationUsage,
        profile: &ProviderProfile,
        limits: &CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<ContextPreviewBound, InvestigationError> {
        let (current, contract) = self.admission_reserve_basis(policy)?;
        let (next, next_contract) = candidate.admission_reserve_basis(policy)?;
        if current["turn_progress"]["admission_mode"] != "choice"
            || contract != next_contract
            || self.payload.system != candidate.payload.system
            || self.payload.spans != candidate.payload.spans
            || self.inspection_requirements != candidate.inspection_requirements
            || !self.compact
            || !candidate.compact
        {
            return Err(InvestigationError::InvalidInput);
        }
        let basis = InvestigationTurnBasis {
            input_ledger_hash: candidate.ledger.fingerprint_with_policy(policy)?,
            previous_ledger: Some(self.ledger.clone()),
            completed_tool: Some(kind.tool()),
        };
        basis.validate_with_policy(&candidate.ledger, policy)?;
        let mode: CompletionCallMode =
            serde_json::from_value(next["turn_progress"]["admission_mode"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let count: u32 =
            serde_json::from_value(next["turn_progress"]["capacity_refusal_count"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let feedback: Option<AdmissionCapacityFeedback> =
            serde_json::from_value(next["turn_progress"]["capacity_feedback"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        let mut progress = Self::progressive_turn_progress(
            &basis,
            &candidate.ledger,
            &next["compact_context"]["query_history"],
            policy,
        )?;
        admission_input::apply_admission_progress(&mut progress, mode, count, &feedback)?;
        if let Some(choices) = &candidate.acquisition_choices {
            if candidate.action_feedback.is_some() {
                return Err(InvestigationError::InvalidInput);
            }
            progress["acquisition_choices"] =
                serde_json::to_value(&**choices).map_err(|_| InvestigationError::InvalidInput)?;
            progress["action_feedback"] = Value::Null;
        }
        // admission_reserve_basis already authenticated the candidate's
        // private correction projection. Include that same field when checking
        // the complete metadata continuation shape for the new role only.
        if SpecialistId::is_typed_citation_prompt(&candidate.payload.system) {
            progress["citation_correction"] = serde_json::to_value(&candidate.citation_correction)
                .map_err(|_| InvestigationError::InvalidInput)?;
        }
        // admission_reserve_basis already validated the candidate's transport
        // against its expected per-call selection, including the @28 visible
        // copy. Growth is accepted only as that same derived schema.
        if let (_, Some(visible)) = response_schema::visible_projection(
            candidate.frozen_specialist()?,
            candidate.payload.response_schema.clone(),
        )? {
            progress["response_schema"] = visible;
        }
        if next["turn_progress"] != progress {
            return Err(InvestigationError::InvalidInput);
        }
        validate_response(candidate, kind, raw_response)?;
        validate_usage_delta(&current, &next, admitted_usage, &candidate.ledger, limits)?;
        validate_metadata_delta(&current, &next, &candidate.ledger, &contract, kind)?;
        validate_prompt_delta(self, candidate, &current, &next, kind)?;

        // These validation projections are no longer used. Do not retain their
        // prompts, contracts or prior ledger through nested firewall reserves.
        drop((
            current,
            next,
            contract,
            next_contract,
            basis,
            progress,
            feedback,
        ));

        let action = candidate.action(action_id.into())?;
        let measurements = ContextPreviewMeasurements {
            current: llm::bounded::measure_bounded_action(
                profile,
                &self.action(action_id.into())?,
                limits,
            )
            .map_err(|_| InvestigationError::LimitExceeded)?,
            candidate: llm::bounded::measure_bounded_action(profile, &action, limits)
                .map_err(|_| InvestigationError::LimitExceeded)?,
            continuation: candidate
                .measure_v9_reserve_with_policy(profile, limits, action_id, policy)?,
        };
        let input_hash = |input: &Self| {
            bounded_bytes(&input.payload, MAX_INPUT_BYTES).map(|bytes| content_hash(&bytes))
        };
        let basis_hash = crate::source_basis::domain_hash(
            b"cartograph:metadata-prompt-preview:v1\0",
            &bounded_bytes(
                &(
                    self.ledger.fingerprint_with_policy(policy)?,
                    input_hash(self)?,
                    candidate.ledger.fingerprint_with_policy(policy)?,
                    input_hash(candidate)?,
                    kind,
                    content_hash(raw_response.as_bytes()),
                    admitted_usage,
                    profile,
                    limits,
                    action_id,
                    policy.profile(),
                ),
                MAX_INPUT_BYTES,
            )?,
        );
        Ok(ContextPreviewBound {
            basis_hash,
            measurements,
        })
    }
}

fn validate_response(
    candidate: &InvestigationInput,
    kind: ContextPreviewKind,
    raw: &str,
) -> Result<(), InvestigationError> {
    let (hash, bytes, cap) = match kind {
        ContextPreviewKind::Query => {
            let manifest = candidate
                .ledger
                .queries
                .last()
                .ok_or(InvestigationError::InvalidInput)?;
            (
                &manifest.response_hash,
                manifest.response_bytes,
                manifest.query.max_bytes,
            )
        }
        ContextPreviewKind::Inventory => {
            let manifest = candidate
                .ledger
                .coverage_reads
                .last()
                .ok_or(InvestigationError::InvalidInput)?;
            // Same typed admission as actual input construction, including the
            // retained inventory body hash and exact qualification identity.
            manifest.admit_response(
                candidate
                    .ledger
                    .coverage
                    .as_ref()
                    .ok_or(InvestigationError::InvalidInput)?,
                raw,
            )?;
            (
                &manifest.response_hash,
                manifest.response_bytes,
                manifest.request.max_bytes,
            )
        }
        ContextPreviewKind::Artifact => {
            let manifest = candidate
                .ledger
                .artifact_reads
                .last()
                .ok_or(InvestigationError::InvalidInput)?;
            manifest.admit_response(
                candidate
                    .ledger
                    .coverage
                    .as_ref()
                    .ok_or(InvestigationError::InvalidInput)?,
                raw,
            )?;
            (
                &manifest.response_hash,
                manifest.response_bytes,
                manifest.request.max_bytes,
            )
        }
    };
    if raw.len() != bytes || raw.len() > cap || content_hash(raw.as_bytes()) != *hash {
        return Err(InvestigationError::InvalidInput);
    }
    strict::preflight(raw, MAX_QUERY_BYTES, 32, 16384, 16384)
}

fn validate_usage_delta(
    current: &Value,
    next: &Value,
    admitted: &InvestigationUsage,
    ledger: &InvestigationInputLedger,
    limits: &CompletionLimits,
) -> Result<(), InvestigationError> {
    let before: InvestigationUsage = serde_json::from_value(current["consumed_budgets"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    let after: InvestigationUsage = serde_json::from_value(next["consumed_budgets"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    let mut expected = before.clone();
    expected.model_invocations = expected
        .model_invocations
        .checked_add(1)
        .ok_or(InvestigationError::LimitExceeded)?;
    expected.generated_token_reservations = expected
        .generated_token_reservations
        .checked_add(limits.values().max_output_tokens)
        .ok_or(InvestigationError::LimitExceeded)?;
    // Observed tokens may be absent on either call; only the exact saved host
    // response can attest them. Present totals cannot decrease.
    for (old, new) in [
        (before.reported_input_tokens, admitted.reported_input_tokens),
        (
            before.reported_output_tokens,
            admitted.reported_output_tokens,
        ),
        (
            before.reported_cache_read_tokens,
            admitted.reported_cache_read_tokens,
        ),
    ] {
        if old.zip(new).is_some_and(|(a, b)| b < a) {
            return Err(InvestigationError::InvalidInput);
        }
    }
    expected.reported_input_tokens = admitted.reported_input_tokens;
    expected.reported_output_tokens = admitted.reported_output_tokens;
    expected.reported_cache_read_tokens = admitted.reported_cache_read_tokens;
    expected.active_milliseconds = admitted.active_milliseconds;
    if expected != *admitted || admitted.active_milliseconds < before.active_milliseconds {
        return Err(InvestigationError::InvalidInput);
    }
    expected.tool_actions = expected
        .tool_actions
        .checked_add(1)
        .ok_or(InvestigationError::LimitExceeded)?;
    expected.selected_facts = ledger.selected_facts.len();
    expected.active_milliseconds = after.active_milliseconds;
    if after != expected || after.active_milliseconds < admitted.active_milliseconds {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn validate_metadata_delta(
    current: &Value,
    next: &Value,
    ledger: &InvestigationInputLedger,
    contract: &InvestigationCompletionContract,
    kind: ContextPreviewKind,
) -> Result<(), InvestigationError> {
    let before: ProtectedCompletionMetadata =
        serde_json::from_value(current["completion_metadata"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
    let after: ProtectedCompletionMetadata =
        serde_json::from_value(next["completion_metadata"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
    let mut expected = before.clone();
    expected.completed_attempt_ids = after.completed_attempt_ids.clone();
    let additional = match kind {
        // Neither a query nor an artifact page discharges an obligation.
        ContextPreviewKind::Query | ContextPreviewKind::Artifact => 0,
        ContextPreviewKind::Inventory => {
            let read = &ledger
                .coverage_reads
                .last()
                .ok_or(InvestigationError::InvalidInput)?
                .request;
            contract.obligations.obligations.iter().filter(|obligation| matches!(obligation,
                obligations::Obligation::Inventory { target, .. }
                if target.context_id == read.context_id && target.repo == read.repo && target.inventory_hash == read.inventory_hash
            )).count()
        }
    };
    if after != expected
        || !after
            .completed_attempt_ids
            .starts_with(&before.completed_attempt_ids)
        || before.completed_attempt_ids.len().checked_add(additional)
            != Some(after.completed_attempt_ids.len())
        || current["turn_progress"]["capacity_refusal_count"]
            != next["turn_progress"]["capacity_refusal_count"]
        || !next["turn_progress"]["capacity_feedback"].is_null()
    {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn validate_prompt_delta(
    base: &InvestigationInput,
    candidate: &InvestigationInput,
    current: &Value,
    next: &Value,
    kind: ContextPreviewKind,
) -> Result<(), InvestigationError> {
    // Compare every unchanged enclosing field, including complete history,
    // targets, question, system-facing policy and all public proof metadata.
    let changes = |name: &str| {
        matches!(
            name,
            "input_ledger_hash"
                | "compact_context"
                | "citations"
                | "consumed_budgets"
                | "turn_progress"
                | "completion_metadata"
                | "coverage_details"
                | "artifact_pages"
        )
    };
    // Supplied artifact-page text follows every other supplied string.
    let pages = |prompt: &Value| prompt["artifact_pages"].as_array().map_or(0, Vec::len);
    let old_pages = pages(current);
    let current_fields = current
        .as_object()
        .ok_or(InvestigationError::InvalidInput)?;
    let next_fields = next.as_object().ok_or(InvestigationError::InvalidInput)?;
    // Compare values and both key sets; do not silently accept added fields.
    if current_fields
        .iter()
        .any(|(name, value)| !changes(name) && next_fields.get(name) != Some(value))
        || next_fields
            .keys()
            .any(|name| !changes(name) && !current_fields.contains_key(name))
    {
        return Err(InvestigationError::InvalidInput);
    }
    match kind {
        ContextPreviewKind::Query => {
            if current["coverage_details"] != next["coverage_details"]
                || current.get("artifact_pages") != next.get("artifact_pages")
                || base.supplied_strings != candidate.supplied_strings
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        ContextPreviewKind::Inventory => {
            let mut old_context = current["compact_context"].clone();
            old_context["query_history"]["registry_hash"] =
                next["compact_context"]["query_history"]["registry_hash"].clone();
            let old_details = current["coverage_details"]
                .as_array()
                .ok_or(InvestigationError::InvalidInput)?;
            let new_details = next["coverage_details"]
                .as_array()
                .ok_or(InvestigationError::InvalidInput)?;
            let added = new_details.last().ok_or(InvestigationError::InvalidInput)?;
            // The inventory text is supplied before any artifact page.
            let split = base
                .supplied_strings
                .len()
                .checked_sub(old_pages)
                .ok_or(InvestigationError::InvalidInput)?;
            let mut expected = base.supplied_strings.clone();
            expected.insert(
                split,
                added["inventory_json"]
                    .as_str()
                    .ok_or(InvestigationError::InvalidInput)?
                    .to_owned(),
            );
            if old_context != next["compact_context"]
                || current["citations"] != next["citations"]
                || current.get("artifact_pages") != next.get("artifact_pages")
                || new_details.len() != old_details.len() + 1
                || !new_details.starts_with(old_details)
                || candidate.supplied_strings != expected
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
        ContextPreviewKind::Artifact => {
            let mut old_context = current["compact_context"].clone();
            old_context["query_history"]["registry_hash"] =
                next["compact_context"]["query_history"]["registry_hash"].clone();
            let empty = Vec::new();
            let old_pages = current["artifact_pages"].as_array().unwrap_or(&empty);
            let new_pages = next["artifact_pages"]
                .as_array()
                .ok_or(InvestigationError::InvalidInput)?;
            let added = new_pages.last().ok_or(InvestigationError::InvalidInput)?;
            if old_context != next["compact_context"]
                || current["citations"] != next["citations"]
                || current["coverage_details"] != next["coverage_details"]
                || new_pages.len() != old_pages.len() + 1
                || !new_pages.starts_with(old_pages)
                || !candidate
                    .supplied_strings
                    .starts_with(&base.supplied_strings)
                || candidate.supplied_strings.len() != base.supplied_strings.len() + 1
                || added["content"].as_str()
                    != candidate.supplied_strings.last().map(String::as_str)
            {
                return Err(InvestigationError::InvalidInput);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "input_context_preview_tests.rs"]
mod tests;
