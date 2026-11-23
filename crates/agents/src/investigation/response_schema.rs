//! Compiled analyst transport hints; host admission remains authoritative.
use super::staged_refusal::CompletionCallMode;
use super::*;
use llm::CompletionPayload;
use serde_json::Value;

#[path = "response_schema_eligibility.rs"]
mod eligibility;
pub(super) use eligibility::{reserve_eligible_acquisition, select_eligible_acquisition};

pub(super) const CHOICE: &str = include_str!("data/analyst-v13-choice.json");
const EXACT_NODE_SCOPE: &str = r#"{"type":"object","properties":{"type":{"const":"node"},"id":{"type":"string","minLength":1,"maxLength":1024}},"required":["type","id"],"additionalProperties":false}"#;
pub(super) const FINISH: &str = include_str!("data/analyst-v13-finish-only.json");

pub(super) fn select(role: SpecialistId, mode: CompletionCallMode) -> Option<&'static str> {
    (role == SpecialistId::DomainAnalystV13).then_some(match mode {
        CompletionCallMode::Choice => CHOICE,
        CompletionCallMode::FinishOnly => FINISH,
    })
}

/// Transport schema for a schema-4 completion role that otherwise sends none
/// (#1989). Without it a provider may wrap the single JSON action in prose,
/// which the strict decoder refuses. The host decoder remains authoritative.
pub(super) fn select_completion(role: SpecialistId) -> Option<String> {
    (role == SpecialistId::SectionWriterV1).then(|| section_writer_schema().to_owned())
}

/// The analyst@13 choice schema narrowed to what section-writer@1 offers:
/// no replay_query action and no proposed_design claims.
fn section_writer_schema() -> &'static str {
    static SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut schema: Value = serde_json::from_str(CHOICE).expect("compiled choice schema");
        schema["anyOf"]
            .as_array_mut()
            .expect("choice actions")
            .retain(|action| action["properties"]["type"]["const"] != "replay_query");
        schema["$defs"]["finding"]["properties"]["claim_kind"]["enum"]
            .as_array_mut()
            .expect("claim kinds")
            .retain(|kind| kind != "proposed_design");
        serde_json::to_string(&schema).expect("schema serializes")
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

/// @28 (#1335): one selection owns both the transport schema and its
/// model-visible copy. The transport becomes the canonical encoding of that
/// copy, so the prompt shows exactly the bytes the provider receives. Every
/// other role returns its selection unchanged and gains no prompt field.
pub(super) fn visible_projection(
    role: SpecialistId,
    selected: Option<String>,
) -> Result<(Option<String>, Option<Value>), InvestigationError> {
    if !role.uses_visible_action_schema() {
        return Ok((selected, None));
    }
    let mut visible: Value = serde_json::from_str(
        selected
            .as_deref()
            .ok_or(InvestigationError::InvalidInput)?,
    )
    .map_err(|_| InvestigationError::InvalidInput)?;
    if role.uses_artifact_reads() {
        offer_artifact_reads(&mut visible)?;
    }
    drop(selected);
    let transport = String::from_utf8(bounded_bytes(&visible, MAX_INPUT_BYTES)?)
        .map_err(|_| InvestigationError::InvalidInput)?;
    Ok((Some(transport), Some(visible)))
}

/// The `read_artifact` action (ADR-0089), offered wherever the choice schema
/// offers `read_coverage_inventory`: an exploration turn, never a pending
/// acquisition choice, a finish-only turn or a finish patch. The host checks
/// the id against its catalogue as well.
const READ_ARTIFACT_ACTION: &str = r#"{"type":"object","properties":{"type":{"const":"read_artifact"},"context_id":{"type":"string"},"artifact_id":{"enum":[]},"expected_content_id":{"anyOf":[{"type":"null"},{"type":"string","minLength":1,"maxLength":96}]},"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":32768}},"required":["type","context_id","artifact_id","expected_content_id","offset","max_bytes"],"additionalProperties":false}"#;

fn offer_artifact_reads(schema: &mut Value) -> Result<(), InvestigationError> {
    let Some(actions) = schema.get_mut("anyOf").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    let Some(inventory) = actions
        .iter()
        .position(|action| action["properties"]["type"]["const"] == "read_coverage_inventory")
    else {
        return Ok(());
    };
    let mut action: Value =
        serde_json::from_str(READ_ARTIFACT_ACTION).map_err(|_| InvestigationError::InvalidInput)?;
    action["properties"]["artifact_id"]["enum"] =
        serde_json::json!(super::artifact_reads::INVESTIGATION_ARTIFACT_IDS);
    actions.insert(inventory + 1, action);
    Ok(())
}

/// Install the @28 copy at `turn_progress.response_schema`, after the stable
/// prefix, and return the transport schema for the same call.
pub(super) fn attach_visible(
    role: SpecialistId,
    prompt: &mut Value,
    selected: Option<String>,
) -> Result<Option<String>, InvestigationError> {
    let (transport, visible) = visible_projection(role, selected)?;
    if let Some(visible) = visible {
        prompt["turn_progress"]
            .as_object_mut()
            .ok_or(InvestigationError::InvalidInput)?
            .insert("response_schema".into(), visible);
    } else if prompt["turn_progress"].get("response_schema").is_some() {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(transport)
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
        slots::<&str>(
            policy
                .limits()
                .evidence_items
                .checked_add(policy.limits().selected_facts)?,
        )?, // @20 citation enum borrowed handles
        MAX_MANIFEST_BYTES,              // bounded borrowed-key encoding
        json_bytes(MAX_MANIFEST_BYTES)?, // decoded exact keys
        json_bytes(MAX_INPUT_BYTES)?,    // finite @18 enum and projection validation
        json_bytes(CHOICE.len().checked_add(EXACT_NODE_SCOPE.len())?)?, // compiled template tree
        // @18 complete replay branches coexist with the specialized schema.
        // Each includes two 64-byte hashes, an index and the full closed shape.
        json_bytes((policy.limits().tool_actions as usize).checked_mul(1024)?)?,
        slots::<QueryHistoryReference>(1)?.checked_add(64)?,
        MAX_INPUT_BYTES, // bounded schema encoding (also on rejection)
        // @28 visible copy: decoded tree plus its canonical transport encoding,
        // both retained while the selection lives (also on rejection).
        json_bytes(MAX_INPUT_BYTES)?,
        MAX_INPUT_BYTES,
        slots::<Vec<u8>>(3)?,
    ])
}

pub(super) fn validate(
    ledger: &InvestigationInputLedger,
    payload: &CompletionPayload,
    prompt: &Value,
    policy: &InvestigationPolicy,
) -> Result<(), InvestigationError> {
    let Some(role) = schema_role(&payload.system) else {
        // Historical non-admission prompts need no mode and never gain a schema.
        return if payload.response_schema.is_none() {
            Ok(())
        } else {
            Err(InvestigationError::InvalidInput)
        };
    };
    ledger.validate_for_specialist(role, policy)?;
    let mode = serde_json::from_value(prompt["turn_progress"]["admission_mode"].clone())
        .map_err(|_| InvestigationError::InvalidInput)?;
    if payload.response_schema != select_admitted(role, mode, ledger)? {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(())
}

/// The typed authority supplies both compact choices and exact action objects.
pub(super) fn select_acquisition_with_policy(
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
    policy: &InvestigationPolicy,
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
                let reference =
                    QueryHistoryReference::from_ledger(request_hash, ledger, index, policy)?;
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

/// Recover from an authenticated unknown ID without inventing another anchor.
/// This is a strict subset of the ordinary schema, so its existing reservation
/// covers it. A fresh all-scope query can discover actual IDs in the admitted
/// context. Historical generic feedback keeps its exact original schema.
pub(super) fn select_acquisition_recovery_with_policy(
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
    feedback: Option<&ActionRefusalFeedback>,
    policy: &InvestigationPolicy,
) -> Result<Option<String>, InvestigationError> {
    let ordinary = select_acquisition_with_policy(mode, choices, ledger, request_hash, policy)?;
    if mode == CompletionCallMode::FinishOnly
        || !choices.attempts_complete()
        || !feedback.is_some_and(|feedback| {
            matches!(
                feedback.reason,
                ActionRefusalReason::UnknownAnchor | ActionRefusalReason::UnknownNode
            )
        })
    {
        return Ok(ordinary);
    }
    let mut schema: Value = serde_json::from_str(
        ordinary
            .as_deref()
            .ok_or(InvestigationError::InvalidInput)?,
    )
    .map_err(|_| InvestigationError::InvalidInput)?;
    schema["$defs"]["query_scope"]["anyOf"] = serde_json::json!([{
        "type":"object", "properties":{"type":{"const":"all"}},
        "required":["type"], "additionalProperties":false
    }]);
    schema["$defs"]["query_cursor"] = serde_json::json!({"type":"null"});
    let narrowed = bounded_bytes(&schema, MAX_INPUT_BYTES)?;
    if narrowed.len()
        > ordinary
            .as_ref()
            .ok_or(InvestigationError::InvalidInput)?
            .len()
    {
        return Err(InvestigationError::InvalidInput);
    }
    String::from_utf8(narrowed)
        .map(Some)
        .map_err(|_| InvestigationError::InvalidInput)
}

// Historical test fixtures explicitly retain their original policy.
#[cfg(test)]
pub(super) fn select_acquisition(
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
) -> Result<Option<String>, InvestigationError> {
    select_acquisition_with_policy(
        mode,
        choices,
        ledger,
        request_hash,
        &INVESTIGATION_V2_POLICY,
    )
}
#[cfg(test)]
pub(super) fn select_acquisition_recovery(
    mode: CompletionCallMode,
    choices: &acquisition_choices::AcquisitionChoices,
    ledger: &InvestigationInputLedger,
    request_hash: &str,
    feedback: Option<&ActionRefusalFeedback>,
) -> Result<Option<String>, InvestigationError> {
    select_acquisition_recovery_with_policy(
        mode,
        choices,
        ledger,
        request_hash,
        feedback,
        &INVESTIGATION_V2_POLICY,
    )
}
