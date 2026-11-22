//! Dedicated @9 input construction. Older builders never select this projection.
use super::*;
use serde_json::json;

impl InvestigationInput {
    /// Builds the @9 query-history input under an explicit policy.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v8_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        request: &obligations::StagedObligationRequest,
        metadata: &staged_refusal::ProtectedCompletionMetadata,
        coverage_details: &[String],
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        let (input, prompt) = Self::prepare_progressive_parts_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
            request,
            metadata,
            policy,
            6,
            &std::collections::BTreeSet::new(),
        )?;
        input.finish_progressive_input(&prompt, coverage_details)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_progressive_parts_with_policy<P: AsRef<str> + serde::Serialize>(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[P],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        request: &obligations::StagedObligationRequest,
        metadata: &staged_refusal::ProtectedCompletionMetadata,
        policy: &InvestigationPolicy,
        request_schema: u32,
        withheld: &std::collections::BTreeSet<String>,
    ) -> Result<(Self, Value), InvestigationError> {
        if !specialist.uses_query_history_disclosure()
            || policy.profile() != INVESTIGATION_V2_POLICY.profile()
        {
            return Err(InvestigationError::InvalidInput);
        }
        let request = request.normalized_with_policy(policy)?;
        request.validate_ledger_with_policy(&ledger, policy)?;
        let selected: SpecialistId = serde_json::from_value(json!(request.specialist_id))
            .map_err(|_| InvestigationError::InvalidInput)?;
        if request.schema_version != request_schema
            || question != request.question
            || selected != specialist
            || ledger
                .coverage
                .as_ref()
                .is_none_or(|proof| proof.context_id != request.expected_context_id)
        {
            return Err(InvestigationError::InvalidInput);
        }
        metadata.validate_with_policy(policy)?;
        let contract = InvestigationCompletionContract {
            schema_version: 1,
            request_hash: request.intent_hash_with_policy(policy)?,
            obligations_hash: request.obligations.fingerprint_with_policy(policy)?,
            obligations: request.obligations.clone(),
        };
        contract.validate_with_policy(policy)?;
        if metadata.request_hash != contract.request_hash
            || metadata.obligations_hash != contract.obligations_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut input = Self::prepare_base_projected(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            policy,
            Some(&contract.request_hash),
            withheld,
        )?;
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let progress = Self::progressive_turn_progress(
            basis,
            &input.ledger,
            &prompt["compact_context"]["query_history"],
            policy,
        )?;
        prompt["schema_version"] = 8.into();
        prompt["policy"] = QUERY_HISTORY_DISCLOSURE_POLICY.into();
        prompt["turn_progress"] = progress;
        let targets = request.obligations.sources_with_policy(policy)?;
        if !targets.is_empty() {
            let targets = normalize_inspection_targets_with_policy(&targets, policy)?;
            prompt["inspection_requirements_hash"] =
                inspection_requirements_hash_with_policy(&targets, policy)?.into();
            prompt["inspection_progress"] = json!({"requirements": targets.iter().map(|target|json!({
                "target_id":target.target_id,
                "acquired":input.ledger.citations.iter().any(|citation|target.matches(citation)),
                "exact_supply":"determined_by_finishing_disclosure"
            })).collect::<Vec<_>>()});
            input.inspection_requirements = Some(targets);
        }
        prompt["completion_contract"] =
            serde_json::to_value(contract).map_err(|_| InvestigationError::InvalidInput)?;
        prompt["completion_metadata"] =
            serde_json::to_value(metadata).map_err(|_| InvestigationError::InvalidInput)?;
        Ok((input, prompt))
    }

    pub(super) fn progressive_turn_progress(
        basis: &InvestigationTurnBasis,
        ledger: &InvestigationInputLedger,
        history: &Value,
        policy: &InvestigationPolicy,
    ) -> Result<Value, InvestigationError> {
        // The legacy progress derivation only needs the last query's cursor.
        // Its durable ordering/counter checks remain authoritative. This scratch
        // structure is never sent, bounded or saved as a legacy projection.
        let mut queries = vec![Value::Null; ledger.queries.len()];
        if let Some(last) = queries.last_mut() {
            *last = json!({"context": history["latest_query"]["context"]});
        }
        let mut progress = serde_json::to_value(basis.progress_with_policy(
            ledger,
            &json!({"queries": queries}),
            policy,
        )?)
        .map_err(|_| InvestigationError::InvalidInput)?;
        progress["contract"] = "investigation-turn-progress@3".into();
        if progress["last_completed_action"]["type"] == "query_context" {
            let id = history["latest_query"]["reference"]["query_id"]
                .as_str()
                .ok_or(InvestigationError::InvalidInput)?;
            let action = progress["last_completed_action"]
                .as_object_mut()
                .ok_or(InvestigationError::InvalidInput)?;
            action.remove("query");
            action.insert("query_id".into(), id.into());
        }
        Ok(progress)
    }

    pub(super) fn finish_progressive_input<P: AsRef<str>>(
        mut self,
        prompt: &Value,
        coverage_details: &[P],
    ) -> Result<Self, InvestigationError> {
        self.payload.prompt = wire::encode_prompt(prompt, MAX_INPUT_BYTES)?;
        self.add_coverage_details_raw(coverage_details)?;
        wire::validate_payload(&self.payload, MAX_INPUT_BYTES)?;
        Ok(self)
    }
}
