//! Compiled analyst transport hints; host admission remains authoritative.
use super::staged_refusal::CompletionCallMode;
use super::*;
use llm::CompletionPayload;
use serde_json::Value;

pub(super) const CHOICE: &str = include_str!("data/analyst-v13-choice.json");
pub(super) const FINISH: &str = include_str!("data/analyst-v13-finish-only.json");

pub(super) fn select(role: SpecialistId, mode: CompletionCallMode) -> Option<&'static str> {
    (role == SpecialistId::DomainAnalystV13).then_some(match mode {
        CompletionCallMode::Choice => CHOICE,
        CompletionCallMode::FinishOnly => FINISH,
    })
}

pub(super) fn validate(
    payload: &CompletionPayload,
    prompt: &Value,
) -> Result<(), InvestigationError> {
    let role = if payload.system == SpecialistId::DomainAnalystV13.prompt() {
        SpecialistId::DomainAnalystV13
    } else {
        // Historical non-admission prompts need no mode and never gain a schema.
        return if payload.response_schema.is_none() {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        };
    };
    let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    if payload.response_schema.as_deref() != select(role, mode) {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}
