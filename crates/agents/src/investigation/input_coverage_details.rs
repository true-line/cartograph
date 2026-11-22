//! Exact acquired detail representations for the qualified prompt.
use super::*;

impl InvestigationInput {
    pub(super) fn add_coverage_details(
        &mut self,
        raw_details: &[String],
    ) -> Result<(), InvestigationError> {
        self.add_coverage_details_raw(raw_details)
    }

    pub(super) fn add_coverage_details_raw<P: AsRef<str>>(
        &mut self,
        raw_details: &[P],
    ) -> Result<(), InvestigationError> {
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        // Artifact pages are supplied by ADR-0089 step 2. Until then a ledger
        // that names one cannot become a prompt, so no page is dropped.
        if raw_details.len() != self.ledger.coverage_reads.len()
            || !self.ledger.artifact_reads.is_empty()
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mut details = Vec::with_capacity(raw_details.len());
        for (manifest, raw) in self.ledger.coverage_reads.iter().zip(raw_details) {
            let detail = manifest.admit_response(proof, raw.as_ref())?;
            strict::preflight(&detail.inventory_json, MAX_QUERY_BYTES, 32, 16384, 16384)?;
            let inventory: Value = serde_json::from_str(&detail.inventory_json)
                .map_err(|_| InvestigationError::InvalidInput)?;
            collect_strings(&inventory, &mut self.graph_strings);
            self.supplied_strings.push(detail.inventory_json.clone());
            details.push(detail);
        }
        let mut prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        prompt["coverage_details"] =
            serde_json::to_value(details).map_err(|_| InvestigationError::InvalidInput)?;
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        Ok(())
    }
}
