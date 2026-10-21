//! Internal prompt9 construction. This representation grants no dispatch or
//! acquisition authority; the host must bind it to its saved admission state.
use super::*;
use serde::{Deserialize, Serialize};
use staged_refusal::{CompletionCallMode, ProtectedCompletionMetadata};

/// A finite byte-capacity reason, independent of completion refusal feedback.
/// Storage and tool/evidence-budget failures retain their terminal semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityRefusalReason {
    PromptCapacity,
    FinishCallReserve,
    FinishTokenReserve,
    UnprovenContribution,
}

impl CapacityRefusalReason {
    // Exhaustive proof table: adding a code requires revisiting its wire name
    // and the maximal variant used by CapacityFeedbackSizeEnvelope.
    fn encoded_name(self) -> &'static str {
        match self {
            Self::PromptCapacity => "prompt_capacity",
            Self::FinishCallReserve => "finish_call_reserve",
            Self::FinishTokenReserve => "finish_token_reserve",
            Self::UnprovenContribution => "unproven_contribution",
        }
    }
}

/// Latest refused action only. Clearing this slot after an accepted acquisition
/// does not reset the separately supplied cumulative refusal count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionCapacityFeedback {
    pub schema_version: u32,
    pub action_hash: String,
    pub reason: CapacityRefusalReason,
}

impl AdmissionCapacityFeedback {
    fn validate(&self) -> Result<(), InvestigationError> {
        if self.schema_version != 1 || !crate::source_basis::hash(&self.action_hash) {
            return Err(InvestigationError::InvalidInput);
        }
        Ok(())
    }
}

const CHOICE_INSTRUCTIONS: &str = "Choose one permitted action. Capacity feedback describes only the latest refused acquisition and grants no evidence or authority. The refusal count persists when an accepted acquisition clears that feedback.";
const FINISH_ONLY_INSTRUCTIONS: &str = "Finish only: return a finish action using the already supplied evidence. Do not query context, read evidence, or read coverage inventory. Capacity feedback grants no evidence or authority.";

impl InvestigationInput {
    /// Internal @10/request7 path. Public start/decode, sealing and worker routing
    /// deliberately remain separate and cannot select this builder implicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_v9_with_policy(
        question: &str,
        specialist: SpecialistId,
        ledger: InvestigationInputLedger,
        query_pages: &[&str],
        evidence: &[AgentEvidence],
        supplied_history: &[String],
        usage: &InvestigationUsage,
        basis: &InvestigationTurnBasis,
        request: &obligations::StagedObligationRequest,
        metadata: &ProtectedCompletionMetadata,
        coverage_details: &[&str],
        mode: CompletionCallMode,
        capacity_refusal_count: u32,
        capacity_feedback: Option<&AdmissionCapacityFeedback>,
        policy: &InvestigationPolicy,
    ) -> Result<Self, InvestigationError> {
        if !specialist.uses_next_prompt_admission()
            || request.schema_version != 7
            || capacity_refusal_count > policy.limits().model_invocations
            || capacity_refusal_count > usage.model_invocations
            || (capacity_feedback.is_some() && capacity_refusal_count == 0)
        {
            return Err(InvestigationError::InvalidInput);
        }
        if let Some(feedback) = capacity_feedback {
            feedback.validate()?;
        }
        if metadata.next_call_mode_with_policy(usage, policy)? != mode {
            return Err(InvestigationError::InvalidInput);
        }
        let (input, mut prompt) = Self::prepare_progressive_parts_with_policy(
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
            7,
        )?;
        prompt["schema_version"] = 9.into();
        apply_admission_progress(
            &mut prompt["turn_progress"],
            mode,
            capacity_refusal_count,
            &capacity_feedback,
        )?;
        input.finish_progressive_input(&prompt, coverage_details)
    }
}

/// Wire ceilings for feedback-only continuations at one exact acquired state.
/// These numbers neither bind a saved host transaction nor authorize dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionReserveMeasurements {
    pub choice: llm::bounded::BoundedWireMeasurements,
    pub finish_only: llm::bounded::BoundedWireMeasurements,
}

impl InvestigationInput {
    /// Measure both instruction variants through the actual bounded wire path.
    /// Graph, query, source, history, target and system strings remain exact.
    /// New acquisitions (including optional cursor/action transitions) require a
    /// fresh prospective input; this is not a bound on their unknown contents.
    /// Reserved replacements contain only decimal numbers, lower-hex hashes and
    /// closed ASCII codes. They add no secret recognizer prefix or private-key
    /// delimiter; quoted JSON boundaries isolate them from unchanged source
    /// strings. The actual redactor is rerun on each complete reserved prompt.
    /// All optional fields that can grow without acquisition are represented:
    /// both feedback slots, refusal id, attempt ids and reported token counts.
    /// Synthetic completion/capacity feedback cannot escape this method as an
    /// InvestigationInput, CompletionAction, sealed receipt or dispatch permit.
    pub fn measure_v9_reserve_with_policy(
        &self,
        profile: &llm::bounded::ProviderProfile,
        limits: &llm::bounded::CompletionLimits,
        action_id: &str,
        policy: &InvestigationPolicy,
    ) -> Result<AdmissionReserveMeasurements, InvestigationError> {
        let (prompt, contract) = self.admission_reserve_basis(policy)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        // The reserve implementation owns the protocol repair allowance.
        // Removing the one final finish opportunity recovers its maximal
        // continuable refusal count without duplicating that allowance here.
        let maximum_refusals = metadata
            .finish_call_reserve_with_policy(policy)?
            .model_invocations
            .checked_add(metadata.refusal_count)
            .and_then(|calls| calls.checked_sub(1))
            .ok_or(InvestigationError::LimitExceeded)?;
        let mut reserved = prompt;
        let completion = reserved["completion_metadata"]
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        completion.insert(
            "completed_attempt_ids".into(),
            serde_json::json!(vec!["f".repeat(64); policy.max_completed_attempts()]),
        );
        completion.insert("refusal_count".into(), maximum_refusals.into());
        completion.insert("last_refusal_id".into(), "f".repeat(64).into());
        completion.insert(
            "feedback".into(),
            serde_json::to_value(
                staged_finish::RefusalFeedback::sizing_upper_envelope_with_policy(
                    &contract.obligations,
                    policy,
                )?,
            )
            .map_err(|_| InvestigationError::InvalidInput)?,
        );
        // Numeric width, not invented usage authority. The actual usage DTO
        // admits two optional token counters which may become present without
        // acquisition; unknown optional/structured fields fail closed.
        let usage = reserved["consumed_budgets"]
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        for (key, value) in usage {
            if value.is_number()
                || matches!(
                    key.as_str(),
                    "reported_input_tokens" | "reported_output_tokens"
                )
            {
                *value = u64::MAX.into();
            } else {
                return Err(InvestigationError::InvalidInput);
            }
        }
        maximize_progress_numbers(&mut reserved["turn_progress"]);
        if [
            CapacityRefusalReason::PromptCapacity,
            CapacityRefusalReason::FinishCallReserve,
            CapacityRefusalReason::FinishTokenReserve,
            CapacityRefusalReason::UnprovenContribution,
        ]
        .iter()
        .any(|reason| {
            reason.encoded_name().len()
                > CapacityRefusalReason::UnprovenContribution
                    .encoded_name()
                    .len()
        }) {
            return Err(InvestigationError::InvalidInput);
        }
        let feedback = CapacityFeedbackSizeEnvelope {
            schema_version: 1,
            action_hash: "f".repeat(64),
            reason: CapacityRefusalReason::UnprovenContribution,
        };
        let mut action = self.action(action_id.into())?;
        let mut measure = |mode| -> Result<_, InvestigationError> {
            let mut prompt = reserved.clone();
            apply_admission_progress(
                &mut prompt["turn_progress"],
                mode,
                policy.limits().model_invocations,
                &Some(&feedback),
            )?;
            wire::replace_prompt(&mut action.payload, &prompt, MAX_INPUT_BYTES)?;
            llm::bounded::measure_bounded_action(profile, &action, limits).map_err(|error| {
                if error.code == llm::bounded::FailureCode::InputTooLarge {
                    InvestigationError::LimitExceeded
                } else {
                    InvestigationError::InvalidInput
                }
            })
        };
        Ok(AdmissionReserveMeasurements {
            choice: measure(CompletionCallMode::Choice)?,
            finish_only: measure(CompletionCallMode::FinishOnly)?,
        })
    }

    fn admission_reserve_basis(
        &self,
        policy: &InvestigationPolicy,
    ) -> Result<(Value, InvestigationCompletionContract), InvestigationError> {
        self.ledger.validate_with_policy(policy)?;
        if policy.profile() != INVESTIGATION_V2_POLICY.profile()
            || ![
                SpecialistId::DomainAnalystV10,
                SpecialistId::EvidenceAuditorV10,
            ]
            .iter()
            .any(|role| self.payload.system == role.prompt())
        {
            return Err(InvestigationError::InvalidInput);
        }
        let prompt: Value = serde_json::from_str(&self.payload.prompt)
            .map_err(|_| InvestigationError::InvalidInput)?;
        let proof = self
            .ledger
            .coverage
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?;
        if prompt["schema_version"] != 9
            || prompt["policy"] != QUERY_HISTORY_DISCLOSURE_POLICY
            || prompt["input_ledger_hash"] != self.ledger.fingerprint_with_policy(policy)?
            || prompt["context_id"] != proof.context_id
            || prompt["coverage_qualification"]
                != serde_json::to_value(&proof.qualification)
                    .map_err(|_| InvestigationError::InvalidInput)?
            || prompt["turn_progress"]["contract"] != "investigation-turn-progress@4"
        {
            return Err(InvestigationError::InvalidInput);
        }
        exact_admission_fields(
            &prompt["turn_progress"],
            &[
                "contract",
                "input_revision",
                "phase",
                "last_completed_action",
                "raw_query_budget",
                "admission_mode",
                "admission_instructions",
                "capacity_refusal_count",
                "capacity_feedback",
            ],
        )?;
        exact_admission_fields(
            &prompt["completion_metadata"],
            &[
                "schema_version",
                "request_hash",
                "obligations_hash",
                "completed_attempt_ids",
                "refusal_count",
                "last_refusal_id",
                "feedback",
            ],
        )?;
        let contract: InvestigationCompletionContract =
            serde_json::from_value(prompt["completion_contract"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        contract.validate_with_policy(policy)?;
        let metadata: ProtectedCompletionMetadata =
            serde_json::from_value(prompt["completion_metadata"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        metadata.validate_with_policy(policy)?;
        let usage: InvestigationUsage = serde_json::from_value(prompt["consumed_budgets"].clone())
            .map_err(|_| InvestigationError::InvalidInput)?;
        if metadata.request_hash != contract.request_hash
            || metadata.obligations_hash != contract.obligations_hash
        {
            return Err(InvestigationError::InvalidInput);
        }
        let mode = metadata.next_call_mode_with_policy(&usage, policy)?;
        let count = prompt["turn_progress"]["capacity_refusal_count"]
            .as_u64()
            .filter(|value| {
                *value <= u64::from(usage.model_invocations)
                    && *value <= u64::from(policy.limits().model_invocations)
            })
            .ok_or(InvestigationError::InvalidInput)? as u32;
        let feedback: Option<AdmissionCapacityFeedback> =
            serde_json::from_value(prompt["turn_progress"]["capacity_feedback"].clone())
                .map_err(|_| InvestigationError::InvalidInput)?;
        if let Some(value) = &feedback {
            value.validate()?;
            if count == 0 {
                return Err(InvestigationError::InvalidInput);
            }
        }
        let mut expected = prompt["turn_progress"].clone();
        apply_admission_progress(&mut expected, mode, count, &feedback)?;
        if expected != prompt["turn_progress"] {
            return Err(InvestigationError::InvalidInput);
        }
        Ok((prompt, contract))
    }
}

/// No conversion to the real capacity DTO: this is serialization data only.
#[derive(Serialize)]
struct CapacityFeedbackSizeEnvelope {
    schema_version: u32,
    action_hash: String,
    reason: CapacityRefusalReason,
}

fn exact_admission_fields(value: &Value, names: &[&str]) -> Result<(), InvestigationError> {
    let object = value.as_object().ok_or(InvestigationError::InvalidInput)?;
    if object.len() != names.len() || object.keys().any(|key| !names.contains(&key.as_str())) {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

fn maximize_progress_numbers(value: &mut Value) {
    match value {
        Value::Number(_) => *value = u64::MAX.into(),
        Value::Array(items) => items.iter_mut().for_each(maximize_progress_numbers),
        Value::Object(fields) => fields.values_mut().for_each(maximize_progress_numbers),
        _ => {}
    }
}

/// The actual builder and a future conservative sizing view use this same
/// enclosing representation. A Serialize value here is not admission authority;
/// this helper is private and only the validated DTO reaches the real builder.
fn apply_admission_progress(
    progress: &mut Value,
    mode: CompletionCallMode,
    count: u32,
    feedback: &impl Serialize,
) -> Result<(), InvestigationError> {
    let fields = progress
        .as_object_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    fields.insert("contract".into(), "investigation-turn-progress@4".into());
    let genuine_phase = match fields.get("last_completed_action") {
        Some(Value::Null) => "initial",
        Some(action) => match action["type"].as_str() {
            Some("query_context") => "after_query",
            Some("read_evidence") => "after_read",
            Some("read_coverage_inventory") => "after_coverage_read",
            _ => return Err(InvestigationError::InvalidInput),
        },
        None => return Err(InvestigationError::InvalidInput),
    };
    let (name, instructions) = match mode {
        CompletionCallMode::Choice => {
            fields.insert("phase".into(), genuine_phase.into());
            ("choice", CHOICE_INSTRUCTIONS)
        }
        CompletionCallMode::FinishOnly => {
            fields.insert("phase".into(), "finish_only".into());
            ("finish_only", FINISH_ONLY_INSTRUCTIONS)
        }
    };
    fields.insert("admission_mode".into(), name.into());
    fields.insert("admission_instructions".into(), instructions.into());
    fields.insert("capacity_refusal_count".into(), count.into());
    fields.insert(
        "capacity_feedback".into(),
        serde_json::to_value(feedback).map_err(|_| InvestigationError::InvalidInput)?,
    );
    Ok(())
}

#[cfg(test)]
#[path = "input_admission_tests.rs"]
mod tests;
