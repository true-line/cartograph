//! One partial component of a future full-workflow byte certificate.
//! This bounds unredacted coverage-detail representation only. It establishes
//! no whole-prompt/redaction/wire fit, runtime admission or dispatch authority.
use super::*;
use context_hub::coverage::CoverageStatus;
use serde::Serialize;

/// Derived from the complete retained archive, never caller byte allowances.
/// Every inventory acquisition may repeat the largest detail. Other prompt
/// fields, unavailable-read/attempt metadata and redaction remain unproven here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowCoverageDetailBound {
    method: &'static str,
    status: &'static str,
    archive_hash: String,
    policy_profile: String,
    maximum_read_count: usize,
    recorded_inventory_count: usize,
    maximum_detail_json_bytes: usize,
    maximum_detail_nested_content_bytes: usize,
    details_array_json_bytes: usize,
    details_array_as_prompt_string_bytes: usize,
}

impl WorkflowCoverageDetailBound {
    /// A failed archive/read/serialization returns an error: an unknown or
    /// oversized response is never silently replaced with an empty detail.
    pub fn derive(
        archive: &InvestigationCoverageArchive,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        if policy.profile() != INVESTIGATION_V2_POLICY.profile() {
            return Err(InvestigationError::InvalidInput);
        }
        archive.validate()?;
        let mut maximum_detail_json_bytes = 0;
        let mut maximum_detail_nested_content_bytes = 0;
        let mut recorded_inventory_count = 0;
        for repo in &archive.proof().qualification.repos {
            if repo.status != CoverageStatus::Recorded {
                continue;
            }
            let detail = archive.read(&InvestigationCoverageRead {
                context_id: archive.proof().context_id.clone(),
                repo: repo.repo.clone(),
                inventory_hash: repo
                    .inventory_hash
                    .clone()
                    .ok_or(InvestigationError::InvalidInput)?,
                max_bytes: MAX_QUERY_BYTES,
            })?;
            let json = String::from_utf8(bounded_bytes(&detail, MAX_QUERY_BYTES)?)
                .map_err(|_| InvestigationError::InvalidInput)?;
            // Exactly the second encoding used when prompt JSON is itself the
            // CompletionPayload.prompt string. Remove only its outer quotes.
            let nested = bounded_bytes(&json, MAX_INPUT_BYTES)?
                .len()
                .checked_sub(2)
                .ok_or(InvestigationError::InvalidInput)?;
            maximum_detail_json_bytes = maximum_detail_json_bytes.max(json.len());
            maximum_detail_nested_content_bytes = maximum_detail_nested_content_bytes.max(nested);
            recorded_inventory_count += 1;
        }
        let maximum_read_count = if recorded_inventory_count == 0 {
            0
        } else {
            usize::try_from(policy.limits().tool_actions)
                .map_err(|_| InvestigationError::LimitExceeded)?
        };
        Ok(Self {
            method: "coverage-detail-repeat-component@1",
            status: "partial_component_only_no_full_workflow_fit_claim",
            archive_hash: core_prov::content_hash(&bounded_bytes(
                archive,
                MAX_INVESTIGATION_COVERAGE_ARCHIVE_BYTES,
            )?),
            policy_profile: policy.profile().into(),
            maximum_read_count,
            recorded_inventory_count,
            maximum_detail_json_bytes,
            maximum_detail_nested_content_bytes,
            details_array_json_bytes: array_bytes(maximum_read_count, maximum_detail_json_bytes)?,
            details_array_as_prompt_string_bytes: array_bytes(
                maximum_read_count,
                maximum_detail_nested_content_bytes,
            )?
            .checked_add(2)
            .ok_or(InvestigationError::LimitExceeded)?,
        })
    }

    /// Includes array brackets and separators, excludes the enclosing field key.
    pub fn details_array_json_bytes(&self) -> usize {
        self.details_array_json_bytes
    }

    /// Includes the prompt string's outer quotes, excludes its other content.
    pub fn details_array_as_prompt_string_bytes(&self) -> usize {
        self.details_array_as_prompt_string_bytes
    }
}

fn array_bytes(count: usize, item: usize) -> Result<usize, InvestigationError> {
    count
        .checked_mul(item)
        .and_then(|bytes| bytes.checked_add(count.saturating_sub(1)))
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or(InvestigationError::LimitExceeded)
}

#[cfg(test)]
#[path = "input_workflow_capacity_tests.rs"]
mod tests;
