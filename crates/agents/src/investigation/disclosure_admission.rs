//! Actual model-visible admission state. Shape is not journal authority.
use super::*;
use staged_refusal::{CompletionCallMode, ProtectedCompletionMetadata};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisclosedAdmissionState {
    pub schema_version: u32,
    pub mode: CompletionCallMode,
    /// Exact consumed budgets printed before this invocation's reservation.
    pub consumed_budgets: InvestigationUsage,
    pub capacity_refusal_count: u32,
    #[serde(deserialize_with = "required_feedback")]
    pub capacity_feedback: Option<AdmissionCapacityFeedback>,
    pub turn_progress_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<AcquisitionAdmission>,
}

/// Reserved representation only. No registered admission family supports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionAdmission {
    pub choices_hash: String,
    pub action_feedback_hash: Option<String>,
}
impl AcquisitionAdmission {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if !crate::source_basis::hash(&self.choices_hash)
            || self
                .action_feedback_hash
                .as_deref()
                .is_some_and(|hash| !crate::source_basis::hash(hash))
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
fn required_feedback<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AdmissionCapacityFeedback>, D::Error> {
    Option::deserialize(deserializer)
}
impl DisclosedAdmissionState {
    /// Structural validation only; the host must compare actual saved journal
    /// state at this invocation's cutoff and authenticate its pre-call usage.
    pub fn validate_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if let Some(acquisition) = &self.acquisition {
            acquisition.validate()?;
            // Future support needs explicit family-bound sealing/replay checks;
            // shape validity alone never admits this reserved extension.
            return Err(InvestigationError::InvalidInput);
        }
        let usage = &self.consumed_budgets;
        let limits = policy.limits();
        if policy.profile() != INVESTIGATION_V2_POLICY.profile()
            || self.schema_version != 1
            || !crate::source_basis::hash(&self.turn_progress_hash)
            || self.capacity_refusal_count > usage.model_invocations
            || usage.model_invocations > limits.model_invocations
            || usage.generated_token_reservations > limits.generated_token_reservations
            || usage.tool_actions > limits.tool_actions
            || usage.selected_facts > limits.selected_facts
            || usage.evidence_requests > limits.evidence_requests
            || usage.evidence_items > limits.evidence_items
            || usage.evidence_bytes > limits.evidence_bytes
            || usage.captured_validation_bytes > limits.captured_validation_bytes
            || (self.capacity_feedback.is_some() && self.capacity_refusal_count == 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(feedback) = &self.capacity_feedback {
            feedback.validate()?;
        }
        Ok(())
    }

    /// Mode depends only on persisted pre-call usage and genuine protected
    /// completion metadata under the saved policy, never current charged usage.
    pub fn validate_with_metadata(
        &self,
        metadata: &ProtectedCompletionMetadata,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        self.validate_with_policy(policy)?;
        if metadata.next_call_mode_with_policy(&self.consumed_budgets, policy)? != self.mode {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}
