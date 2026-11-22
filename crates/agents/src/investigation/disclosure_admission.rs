//! Actual model-visible admission state. Shape is not journal authority.
#![allow(
    missing_docs,
    reason = "undocumented before workspace lints (#1764): document and delete"
)]
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

/// Shape-only stamp. Its hashes require separate saved-cutoff authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionAdmission {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_patch: Option<finish_patch::PatchBasis>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub finish_policy_complete: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub capacity_exhausted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<ActionRefusalReason>,
    pub choices_hash: String,
    pub action_feedback_hash: Option<String>,
}
fn is_false(value: &bool) -> bool {
    !*value
}

impl AcquisitionAdmission {
    pub fn validate(&self) -> Result<(), InvestigationError> {
        if let Some(patch) = &self.finish_patch {
            patch
                .validate()
                .map_err(|_| InvestigationError::InvalidInput)?;
        }
        if self
            .finish_reason
            .is_some_and(|reason| reason != ActionRefusalReason::RepeatedRead)
            || (self.finish_reason.is_some() && self.action_feedback_hash.is_none())
            || !crate::source_basis::hash(&self.choices_hash)
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
/// Canonical journal feedback; input publication remains separately gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRefusalReason {
    ActionNotOffered,
    RepeatedRead,
    QueryRejected,
    UnknownAnchor,
    UnknownNode,
    CursorSnapshot,
    CursorSelection,
    CursorContext,
    CursorOffset,
    QueryLimit,
    QueryPageBudget,
    /// @29: the tool call did not decode to an action. It is returned to the
    /// model, not run, at most [`MAX_INVALID_ACTION_REFUSALS`] times a run.
    InvalidAction,
}
/// @29: undecodable tool calls a run returns to the model before the next one
/// ends it (AC-1420).
pub const MAX_INVALID_ACTION_REFUSALS: usize = 2;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionRefusalFeedback {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_budget: Option<query_refusal::QueryBudgetDiagnostic>,
    pub reason: ActionRefusalReason,
    pub invocation_ordinal: u32,
    pub action_hash: String,
    pub choices_hash: String,
}
impl ActionRefusalFeedback {
    pub fn stamp_hash(&self) -> Result<String, InvestigationError> {
        if (self.reason == ActionRefusalReason::QueryPageBudget) != self.query_budget.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(budget) = self.query_budget {
            budget.validate()?;
        }
        if self.invocation_ordinal == 0
            || !crate::source_basis::hash(&self.action_hash)
            || !crate::source_basis::hash(&self.choices_hash)
        {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(core_prov::content_hash(&bounded_bytes(
            self,
            MAX_ACTION_BYTES,
        )?))
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
        self.validate_shape_with_policy(policy)
    }

    /// Saved-call structural check only. The host must obtain the role from
    /// validated saved task context; this does not authenticate either hash.
    pub fn validate_saved_role_shape(
        &self,
        role: SpecialistId,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        if role.uses_acquisition_choices() != self.acquisition.is_some() {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(acquisition) = &self.acquisition {
            acquisition.validate()?;
            if acquisition.finish_patch.is_some() && !role.uses_finish_patch() {
                return Err(InvestigationError::InvalidInput);
            }
            if acquisition.finish_policy_complete && !policy.local_finish_generation() {
                return Err(InvestigationError::InvalidInput);
            }
            if acquisition.capacity_exhausted && !role.uses_capacity_exclusions() {
                return Err(InvestigationError::InvalidInput);
            }
        }
        self.validate_shape_with_policy(policy)
    }

    fn validate_shape_with_policy(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
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

    /// Shape-only mode check. The host must authenticate finish_reason against
    /// the exact saved refusal prefix; this stamp alone grants no authority.
    pub fn validate_mode_with_metadata(
        &self,
        metadata: &ProtectedCompletionMetadata,
        policy: &InvestigationPolicy,
    ) -> Result<(), InvestigationError> {
        let expected = if self.acquisition.as_ref().is_some_and(|stamp| {
            stamp.finish_patch.is_some()
                || stamp.finish_policy_complete
                || stamp.capacity_exhausted
                || stamp.finish_reason == Some(ActionRefusalReason::RepeatedRead)
        }) {
            CompletionCallMode::FinishOnly
        } else {
            metadata.next_call_mode_with_policy(&self.consumed_budgets, policy)?
        };
        if self
            .acquisition
            .as_ref()
            .and_then(|s| s.finish_patch.as_ref())
            .is_some_and(|p| {
                metadata.refusal_count != 1
                    || metadata.last_refusal_id.as_ref() != Some(&p.refusal_hash)
            })
            || expected != self.mode
        {
            return Err(InvestigationError::InvalidInput);
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
