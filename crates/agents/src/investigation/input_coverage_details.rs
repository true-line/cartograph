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
        // Retained bodies in ledger order: every coverage read, then every
        // artifact page (ADR-0089). A missing or extra body is refused, so no
        // acquired page is dropped from the prompt.
        let coverage_count = self.ledger.coverage_reads.len();
        if raw_details.len()
            != coverage_count
                .checked_add(self.ledger.artifact_reads.len())
                .ok_or(InvestigationError::InvalidInput)?
        {
            return Err(InvestigationError::InvalidInput);
        }
        let (raw_details, raw_pages) = raw_details.split_at(coverage_count);
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
        // Untrusted compiled-artifact text: supplied and firewall-checked like
        // other supplied strings, never a citation. Absent without reads, so
        // every older prompt keeps its bytes.
        if !raw_pages.is_empty() {
            let mut pages = Vec::with_capacity(raw_pages.len());
            for (manifest, raw) in self.ledger.artifact_reads.iter().zip(raw_pages) {
                let page = manifest.admit_response(proof, raw.as_ref())?;
                self.supplied_strings.push(page.content.clone());
                pages.push(page);
            }
            prompt["artifact_pages"] =
                serde_json::to_value(pages).map_err(|_| InvestigationError::InvalidInput)?;
        }
        wire::replace_prompt(&mut self.payload, &prompt, MAX_INPUT_BYTES)?;
        Ok(())
    }
}
