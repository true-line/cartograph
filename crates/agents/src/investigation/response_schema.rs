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

/// @14/@15 specialize only source fact identity; receipt guards still own ranges.
/// All callers supply the validated private ledger, never a prompt projection.
pub(super) fn select_admitted(
    role: SpecialistId,
    mode: CompletionCallMode,
    ledger: &InvestigationInputLedger,
) -> Result<Option<String>, InvestigationError> {
    if !matches!(
        role,
        SpecialistId::DomainAnalystV14
            | SpecialistId::DomainAnalystV15
            | SpecialistId::DomainAnalystV16
    ) {
        return Ok(select(role, mode).map(str::to_owned));
    }
    if mode == CompletionCallMode::FinishOnly {
        if role == SpecialistId::DomainAnalystV16 {
            let schema: Value =
                serde_json::from_str(FINISH).map_err(|_| InvestigationError::InvalidInput)?;
            return String::from_utf8(bounded_bytes(&schema, MAX_INPUT_BYTES)?)
                .map(Some)
                .map_err(|_| InvestigationError::InvalidInput);
        }
        return Ok(Some(FINISH.to_owned()));
    }
    let facts: Vec<_> = ledger
        .selected_facts
        .iter()
        .filter(|selection| selection.binding.is_some())
        .map(|selection| &selection.fact)
        .collect();
    let keys = bounded_bytes(&facts, MAX_MANIFEST_BYTES)?;
    let mut schema: Value =
        serde_json::from_str(CHOICE).map_err(|_| InvestigationError::InvalidInput)?;
    let actions = schema["anyOf"]
        .as_array_mut()
        .ok_or(InvestigationError::InvalidInput)?;
    let ordinal = actions
        .iter()
        .position(|action| action["properties"]["type"]["const"] == "read_evidence")
        .ok_or(InvestigationError::InvalidInput)?;
    if facts.is_empty() {
        actions.remove(ordinal);
    } else {
        let mut fact_schema = serde_json::Map::new();
        fact_schema.insert(
            "enum".into(),
            serde_json::from_slice::<Value>(&keys).map_err(|_| InvestigationError::InvalidInput)?,
        );
        actions[ordinal]["properties"]["fact"] = Value::Object(fact_schema);
    }
    String::from_utf8(bounded_bytes(&schema, MAX_INPUT_BYTES)?)
        .map(Some)
        .map_err(|_| InvestigationError::InvalidInput)
}

pub(super) fn schema_role(system: &str) -> Option<SpecialistId> {
    [
        SpecialistId::DomainAnalystV13,
        SpecialistId::DomainAnalystV14,
        SpecialistId::DomainAnalystV15,
        SpecialistId::DomainAnalystV16,
    ]
    .into_iter()
    .find(|role| system == role.prompt())
}

/// One selection owns both transport and @16's model-visible schema object.
pub(super) fn select_for_prompt(
    role: SpecialistId,
    mode: CompletionCallMode,
    ledger: &InvestigationInputLedger,
    prompt: &mut Value,
) -> Result<Option<String>, InvestigationError> {
    let selected = select_admitted(role, mode, ledger)?;
    if role == SpecialistId::DomainAnalystV16 {
        prompt["response_schema"] = serde_json::from_str(
            selected
                .as_deref()
                .ok_or(InvestigationError::InvalidInput)?,
        )
        .map_err(|_| InvestigationError::InvalidInput)?;
    }
    Ok(selected)
}

/// Additional selector owners, including rejected growth before final wire admission.
pub(super) fn workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
    use context_hub::logical::{json_bytes, slots, sum};
    sum(&[
        super::task_validation_workspace::definition_workspace(SpecialistId::DomainAnalystV16)?,
        slots::<&crate::TaskFactKey>(policy.limits().selected_facts)?,
        MAX_MANIFEST_BYTES,              // bounded borrowed-key encoding
        json_bytes(MAX_MANIFEST_BYTES)?, // decoded exact keys
        json_bytes(CHOICE.len())?,       // compiled template tree
        json_bytes(MAX_INPUT_BYTES)?,    // visible selected-schema parse, also on rejection
        MAX_INPUT_BYTES,                 // bounded schema encoding (also on rejection)
        MAX_INPUT_BYTES,                 // visible canonical encoding while selection lives
        slots::<Vec<u8>>(3)?,
    ])
}

pub(super) fn validate(
    ledger: &InvestigationInputLedger,
    payload: &CompletionPayload,
    prompt: &Value,
) -> Result<(), InvestigationError> {
    let Some(role) = schema_role(&payload.system) else {
        // Historical non-admission prompts need no mode and never gain a schema.
        return if payload.response_schema.is_none() && prompt.get("response_schema").is_none() {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        };
    };
    let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    let selected = select_admitted(role, mode, ledger)?;
    if payload.response_schema != selected {
        return Err(InvestigationError::InvalidInput);
    }
    if role == SpecialistId::DomainAnalystV16 {
        let visible = prompt
            .get("response_schema")
            .ok_or(InvestigationError::InvalidInput)?;
        if bounded_bytes(visible, MAX_INPUT_BYTES)?
            != selected
                .as_deref()
                .ok_or(InvestigationError::InvalidInput)?
                .as_bytes()
        {
            return Err(InvestigationError::InvalidInput);
        }
    } else if prompt.get("response_schema").is_some() {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}
