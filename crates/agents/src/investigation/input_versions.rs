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
        if !specialist.uses_turn_progress() || specialist.uses_inspection_requirements() {
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
        let mut input = Self::prepare_base(
            question,
            specialist,
            ledger,
            query_pages,
            evidence,
            supplied_history,
            usage,
        )?;
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let progress = basis.progress(&input.ledger, &prompt["compact_context"])?;
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
        let mut prompt: Value = serde_json::from_str(&input.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["schema_version"] = 4.into();
        prompt["inspection_requirements_hash"] = inspection_requirements_hash(&targets)?.into();
        prompt["inspection_requirements"] =
            serde_json::to_value(&targets).map_err(|_| InvestigationError::InvalidInput)?;
        prompt["inspection_progress"] = serde_json::json!({"requirements":targets.iter().map(|target|serde_json::json!({
            "target_id":target.target_id,"acquired":input.ledger.citations.iter().any(|citation|target.matches(citation)),
            "exact_supply":"determined_by_finishing_disclosure"})).collect::<Vec<_>>()});
        input.payload.prompt = String::from_utf8(bounded_bytes(&prompt, MAX_INPUT_BYTES)?)
            .map_err(|_| InvestigationError::InvalidInput)?;
        bounded_bytes(&input.payload, MAX_INPUT_BYTES)?;
        input.inspection_requirements = Some(targets);
        Ok(input)
    }
}
