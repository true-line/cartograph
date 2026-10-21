use super::*;
impl InvestigationInput {
    /// V4 requires a coherently read durable turn basis; legacy inputs never
    /// acquire progress fields or change their serialized representation.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v4(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_turn_progress()
            || specialist.uses_inspection_requirements()
            || specialist.uses_coverage_qualification()
        {
            return Err(InvestigationError::InvalidInput);
        }
        Self::prepare_progress(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_progress(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
    ) -> Result<Self, InvestigationError> {
        Self::prepare_progress_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
            &INVESTIGATION_V1_POLICY,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_progress_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        let mut input = Self::prepare_base_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            policy,
        )?;
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let progress =
            basis.progress_with_policy(&input.ledger, &prompt["compact_context"], policy)?;
        prompt["schema_version"] = 3.into();
        prompt["turn_progress"] =
            serde_json::to_value(progress).map_err(|_| InvestigationError::InvalidInput)?;
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&input.payload, MAX_INPUT_BYTES)?;
        Ok(input)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v5(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        targets: &[InvestigationInspectionTarget],
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_inspection_requirements() {
            return Err(InvestigationError::InvalidInput);
        }
        let targets = normalize_inspection_targets(targets)?;
        if targets[0].graph_snapshot_id != ledger.graph_snapshot_id {
            return Err(InvestigationError::InvalidInput);
        }
        let mut input = Self::prepare_progress(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
        )?;
        input.add_inspection_requirements(targets)?;
        Ok(input)
    }

    /// Coverage-qualified compact/progress input. None is coverage-only; a
    /// supplied target slice must normalize to a nonempty exact selection.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v6(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[String],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        targets: Option<&[InvestigationInspectionTarget]>,
        coverage_details: &[String],
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_coverage_qualification() || specialist.uses_completion_obligations() {
            return Err(InvestigationError::InvalidInput);
        }
        let targets = targets.map(normalize_inspection_targets).transpose()?;
        if targets
            .as_ref()
            .is_some_and(|targets| targets[0].graph_snapshot_id != ledger.graph_snapshot_id)
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut input = Self::prepare_progress(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
        )?;
        if let Some(targets) = targets {
            input.add_inspection_requirements(targets)?;
        }
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["schema_version"] = 5.into();
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&input.payload, MAX_INPUT_BYTES)?;
        input.add_coverage_details(coverage_details)?;
        Ok(input)
    }

    /// V7 binds the immutable mixed contract and bounded durable feedback.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v7(
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
    ) -> Result<Self, InvestigationError> {
        Self::prepare_v7_with_policy(
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
            coverage_details,
            &INVESTIGATION_V1_POLICY,
        )
    }

    /// Trusted host route; the immutable policy must match the request family.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v7_with_policy(
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
        // Newer disclosure families must use their own builder. In particular,
        // policy identity alone cannot select an older model-visible prompt.
        if !specialist.uses_completion_obligations() || specialist.uses_query_history_disclosure() {
            return Err(InvestigationError::InvalidInput);
        }
        let request = request.normalized_with_policy(policy)?;
        request.validate_ledger_with_policy(&ledger, policy)?;
        let selected: SpecialistId =
            serde_json::from_value(serde_json::Value::String(request.specialist_id.clone()))
                .map_err(|_| InvestigationError::InvalidInput)?;
        if selected != specialist
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
        let targets = request.obligations.sources_with_policy(policy)?;
        let mut input = Self::prepare_progress_with_policy(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
            basis,
            policy,
        )?;
        if !targets.is_empty() {
            input.add_inspection_requirements_with_policy(
                normalize_inspection_targets_with_policy(&targets, policy)?,
                policy,
            )?;
        }
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        // Coverage details validates the raw acquired responses before schema6.
        prompt["schema_version"] = 5.into();
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        input.add_coverage_details(coverage_details)?;
        prompt = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["schema_version"] = if policy.profile() == INVESTIGATION_V2_POLICY.profile() {
            7.into()
        } else {
            6.into()
        };
        prompt["completion_contract"] =
            serde_json::to_value(contract).map_err(|_| InvestigationError::InvalidInput)?;
        prompt["completion_metadata"] =
            serde_json::to_value(metadata).map_err(|_| InvestigationError::InvalidInput)?;
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&input.payload, MAX_INPUT_BYTES)?;
        Ok(input)
    }

    fn add_inspection_requirements(
        &mut self,
        targets: Vec<InvestigationInspectionTarget>,
    ) -> Result<(), InvestigationError> {
        self.add_inspection_requirements_with_policy(targets, &INVESTIGATION_V1_POLICY)
    }
    fn add_inspection_requirements_with_policy(
        &mut self,
        targets: Vec<InvestigationInspectionTarget>,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["schema_version"] = 4.into();
        prompt["inspection_requirements_hash"] =
            inspection_requirements_hash_with_policy(&targets, policy)?.into();
        // V2 supplies these exact descriptors once in completion_contract.
        // Keep the private targets and their hash for finishing disclosure.
        if policy.profile() != INVESTIGATION_V2_POLICY.profile() {
            prompt["inspection_requirements"] =
                serde_json::to_value(&targets).map_err(|_| InvestigationError::InvalidInput)?;
        }
        prompt["inspection_progress"] = serde_json::json!({"requirements":targets.iter().map(|target|serde_json::json!({
            "target_id":target.target_id,"acquired":self.ledger.citations.iter().any(|citation|target.matches(citation)),
            "exact_supply":"determined_by_finishing_disclosure"})).collect::<Vec<_>>()});
        self.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&self.payload, MAX_INPUT_BYTES)?;
        self.inspection_requirements = Some(targets);
        Ok(())
    }
}

#[cfg(test)]
#[path = "input_coverage_tests.rs"]
mod coverage_tests;
