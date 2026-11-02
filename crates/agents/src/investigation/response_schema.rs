//! Compiled analyst transport hints; host admission remains authoritative.
use super::staged_refusal::CompletionCallMode;
use super::*;
use llm::CompletionPayload;
use serde_json::Value;

pub(super) const CHOICE: &str = include_str!("data/analyst-v13-choice.json");
const EXACT_NODE_SCOPE: &str = r#"{"type":"object","properties":{"type":{"const":"node"},"id":{"type":"string","minLength":1,"maxLength":1024}},"required":["type","id"],"additionalProperties":false}"#;
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
            | SpecialistId::DomainAnalystV17
    ) {
        return Ok(select(role, mode).map(str::to_owned));
    }
    if mode == CompletionCallMode::FinishOnly {
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
    if role.uses_exact_node_queries() {
        schema["$defs"]["query_scope"]["anyOf"]
            .as_array_mut()
            .ok_or(InvestigationError::InvalidInput)?
            .push(
                serde_json::from_str(EXACT_NODE_SCOPE)
                    .map_err(|_| InvestigationError::InvalidInput)?,
            );
    }
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
        SpecialistId::DomainAnalystV17,
    ]
    .into_iter()
    .find(|role| system == role.prompt())
}

/// Additional selector owners, including rejected growth before final wire admission.
pub(super) fn workspace_bytes(policy: &InvestigationPolicy) -> Option<usize> {
    use context_hub::logical::{json_bytes, slots, sum};
    sum(&[
        super::task_validation_workspace::definition_workspace(SpecialistId::DomainAnalystV17)?,
        slots::<&crate::TaskFactKey>(policy.limits().selected_facts)?,
        MAX_MANIFEST_BYTES,              // bounded borrowed-key encoding
        json_bytes(MAX_MANIFEST_BYTES)?, // decoded exact keys
        json_bytes(MAX_INPUT_BYTES)?,    // finite @18 enum and projection validation
        json_bytes(CHOICE.len().checked_add(EXACT_NODE_SCOPE.len())?)?, // compiled template tree
        // @18 complete replay branches coexist with the specialized schema.
        // Each includes two 64-byte hashes, an index and the full closed shape.
        json_bytes((policy.limits().tool_actions as usize).checked_mul(1024)?)?,
        slots::<QueryHistoryReference>(1)?.checked_add(64)?,
        MAX_INPUT_BYTES, // bounded schema encoding (also on rejection)
        slots::<Vec<u8>>(2)?,
    ])
}

pub(super) fn validate(
    ledger: &InvestigationInputLedger,
    payload: &CompletionPayload,
    prompt: &Value,
) -> Result<(), InvestigationError> {
    let Some(role) = schema_role(&payload.system) else {
        // Historical non-admission prompts need no mode and never gain a schema.
        return if payload.response_schema.is_none() {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        };
    };
    ledger.validate_for_specialist(role, &INVESTIGATION_V2_POLICY)?;
    let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    if payload.response_schema != select_admitted(role, mode, ledger)? {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

/// The typed authority supplies both compact choices and exact action objects.
pub(super) fn select_acquisition(
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
) -> Result<Option<String>, InvestigationError> {
    if mode == CompletionCallMode::FinishOnly {
        return Ok(Some(FINISH.into()));
    }
    if choices.attempts_complete() {
        let selected = select_admitted(SpecialistId::DomainAnalystV17, mode, ledger)?;
        // Inventory/source attempts do not create query manifests. Keep the
        // historical @17 schema intact and narrow only current @18 exploration.
        let mut schema: Value = serde_json::from_str(
            selected
                .as_deref()
                .ok_or(InvestigationError::InvalidInput)?,
        )
        .map_err(|_| InvestigationError::InvalidInput)?;
        drop(selected);
        let actions = schema["anyOf"]
            .as_array_mut()
            .ok_or(InvestigationError::InvalidInput)?;
        let replay = actions
            .iter()
            .position(|action| action["properties"]["type"]["const"] == "replay_query")
            .ok_or(InvestigationError::InvalidInput)?;
        actions.remove(replay);
        if !ledger.queries.is_empty() {
            let context = &ledger
                .coverage
                .as_ref()
                .ok_or(InvestigationError::InvalidInput)?
                .context_id;
            for index in 0..ledger.queries.len() {
                let reference = QueryHistoryReference::from_ledger(
                    request_hash,
                    ledger,
                    index,
                    &INVESTIGATION_V2_POLICY,
                )?;
                // Ollama converts anyOf before sibling properties: each
                // alternative must carry the complete required action shape.
                let branch = serde_json::json!({
                    "type":"object",
                    "properties": {
                        "type":{"const":"replay_query"},
                        "context_id":{"const":context},
                        "query_id":{"const":reference.query_id},
                        "query_index":{"const":reference.query_index}
                    },
                    "required":["type","context_id","query_id","query_index"],
                    "additionalProperties":false
                });
                bounded_bytes(&branch, 1024)?;
                actions.push(branch);
            }
        }
        return String::from_utf8(bounded_bytes(&schema, MAX_INPUT_BYTES)?)
            .map(Some)
            .map_err(|_| InvestigationError::InvalidInput);
    }
    if choices.entries().is_empty() {
        return Ok(Some(FINISH.into()));
    }
    let mut schema: Value =
        serde_json::from_str(FINISH).map_err(|_| InvestigationError::InvalidInput)?;
    let actions = choices
        .entries()
        .iter()
        .map(|entry| &entry.action)
        .collect::<Vec<_>>();
    schema["anyOf"]
        .as_array_mut()
        .ok_or(InvestigationError::InvalidInput)?
        .push(serde_json::json!({"enum":actions}));
    String::from_utf8(bounded_bytes(&schema, MAX_INPUT_BYTES)?)
        .map(Some)
        .map_err(|_| InvestigationError::InvalidInput)
}
