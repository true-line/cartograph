//! ADR-0085: a role's actions offered as native tools (AC-1420, AC-1421).
//!
//! Tool definitions are generated from the same admitted per-turn response
//! schema the bounded loop sends, so a role's narrowing (exact node scopes,
//! no replay for section-writer) carries over unchanged. A tool call is turned
//! back into the bounded loop's exact action bytes and decoded by the existing
//! decoder: the host never trusts a second, tool-specific parser. Listing a
//! tool grants nothing; admission stays authoritative.
use super::staged_refusal::CompletionCallMode;
use super::*;
use serde::Serialize;
use serde_json::{Map, Value};

/// Closed tool vocabulary. Names equal the bounded loop's action `type`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTool {
    /// Bounded graph query in the frozen scope.
    QueryContext,
    /// Bounded page of one repository's coverage inventory.
    ReadCoverageInventory,
    /// Exact source bytes of one admitted fact's evidence option.
    ReadEvidence,
    /// Re-run a saved query by reference.
    ReplayQuery,
    /// Terminal findings or insufficient evidence.
    Finish,
}

impl AgentTool {
    /// Every tool, in vocabulary order.
    pub const ALL: [Self; 5] = [
        Self::QueryContext,
        Self::ReadCoverageInventory,
        Self::ReadEvidence,
        Self::ReplayQuery,
        Self::Finish,
    ];

    /// Stable tool name, equal to the bounded action `type`.
    pub fn name(self) -> &'static str {
        match self {
            Self::QueryContext => "query_context",
            Self::ReadCoverageInventory => "read_coverage_inventory",
            Self::ReadEvidence => "read_evidence",
            Self::ReplayQuery => "replay_query",
            Self::Finish => "finish",
        }
    }

    /// The tool with this exact name, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    fn description(self) -> &'static str {
        match self {
            Self::QueryContext => {
                "Query the recovered graph within this investigation's frozen scope. Returns a bounded page of facts, their evidence options and a next_cursor; continue only with the exact returned cursor and unchanged selection."
            }
            Self::ReadCoverageInventory => {
                "Read a bounded page of one repository's coverage inventory named by the coverage qualification."
            }
            Self::ReadEvidence => {
                "Acquire the exact source bytes of one evidence option of an already admitted fact, by its original role and index. Its citation names bytes actually supplied."
            }
            Self::ReplayQuery => {
                "Run a saved historical query again by its query_index and query_id. Consumes normal budgets."
            }
            Self::Finish => {
                "End the investigation with findings that cite only citations acquired in this run, or with insufficient_evidence and explicit limitations. Refused until the run has read source evidence for any supported finding."
            }
        }
    }
}

/// One tool as sent to a runtime: name, description and argument JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDefinition {
    /// Tool name sent to the runtime.
    pub name: &'static str,
    /// Model-facing description.
    pub description: &'static str,
    /// Argument JSON Schema, with the source schema's `$defs`.
    pub input_schema: Value,
    /// Whether the host injects `context_id`; the model never supplies it.
    #[serde(skip)]
    host_context_id: bool,
}

/// Why a tool call was refused before or at admission. Returned to the model
/// as a tool error; the run continues (AC-1420).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolRefusal {
    /// Stable refusal code.
    pub code: &'static str,
    /// What the model should do instead.
    pub prescription: String,
}

impl ToolRefusal {
    fn new(code: &'static str, prescription: impl Into<String>) -> Self {
        Self {
            code,
            prescription: prescription.into(),
        }
    }
}

/// Tools for `role` this turn, from its admitted choice schema. `None` when the
/// role has no compiled schema and so cannot run on a tool-calling runtime.
pub fn tool_definitions_for(
    role: SpecialistId,
    ledger: &InvestigationInputLedger,
) -> Result<Option<Vec<ToolDefinition>>, InvestigationError> {
    response_schema::select_admitted(role, CompletionCallMode::Choice, ledger)?
        .map(|schema| tool_definitions_from_schema(&schema))
        .transpose()
}

/// Splits a choice schema (`anyOf` of actions plus the two finish wrappers)
/// into one tool per action. `$defs` are copied so every `$ref` resolves.
pub fn tool_definitions_from_schema(
    schema: &str,
) -> Result<Vec<ToolDefinition>, InvestigationError> {
    let schema: Value =
        serde_json::from_str(schema).map_err(|_| InvestigationError::InvalidInput)?;
    let defs = schema
        .get("$defs")
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let branches = schema["anyOf"]
        .as_array()
        .ok_or(InvestigationError::InvalidInput)?;
    let mut tools = Vec::new();
    let mut finish = Vec::new();
    for branch in branches {
        if let Some(reference) = branch.get("$ref").and_then(Value::as_str) {
            let name = reference
                .strip_prefix("#/$defs/")
                .ok_or(InvestigationError::InvalidInput)?;
            let wrapper = defs.get(name).ok_or(InvestigationError::InvalidInput)?;
            let action = wrapper["properties"]["action"]["$ref"]
                .as_str()
                .ok_or(InvestigationError::InvalidInput)?;
            finish.push(serde_json::json!({ "$ref": action }));
            continue;
        }
        if let Some(values) = branch.get("enum") {
            // Pending acquisition choices: one `enum` of concrete actions.
            enum_tools(values, &mut tools)?;
            continue;
        }
        let name = branch["properties"]["type"]["const"]
            .as_str()
            .ok_or(InvestigationError::InvalidInput)?;
        let tool = AgentTool::from_name(name).ok_or(InvestigationError::InvalidInput)?;
        let mut input = branch.clone();
        let host_context_id = strip_host_fields(&mut input);
        input["$defs"] = defs.clone();
        tools.push(ToolDefinition {
            name: tool.name(),
            description: tool.description(),
            input_schema: input,
            host_context_id,
        });
    }
    if !finish.is_empty() {
        // The finish argument is the bare finish action; the host adds the
        // wrapper and `type`. Each variant drops its `type` const the same way.
        let mut variants = Vec::new();
        for reference in finish {
            let name = reference["$ref"]
                .as_str()
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .ok_or(InvestigationError::InvalidInput)?;
            let mut variant = defs
                .get(name)
                .cloned()
                .ok_or(InvestigationError::InvalidInput)?;
            strip_host_fields(&mut variant);
            variants.push(variant);
        }
        tools.push(ToolDefinition {
            name: AgentTool::Finish.name(),
            description: AgentTool::Finish.description(),
            input_schema: serde_json::json!({
                "type": "object",
                "anyOf": variants,
                "$defs": defs,
            }),
            host_context_id: false,
        });
    }
    Ok(tools)
}

/// Splits an `enum` of concrete acquisition actions (the choice schema while
/// acquisition choices are pending) into one tool per action `type`, each
/// accepting only its own exact actions. `type` and `context_id` are host
/// fields as elsewhere, so a tool whose actions carry `context_id` needs them
/// all to carry it. A type that already has a tool is refused, never merged.
fn enum_tools(values: &Value, tools: &mut Vec<ToolDefinition>) -> Result<(), InvestigationError> {
    let values = values.as_array().ok_or(InvestigationError::InvalidInput)?;
    let mut groups: Vec<(AgentTool, Vec<Value>, Option<bool>)> = Vec::new();
    for value in values {
        let mut action = value
            .as_object()
            .cloned()
            .ok_or(InvestigationError::InvalidInput)?;
        let tool = action
            .remove("type")
            .as_ref()
            .and_then(Value::as_str)
            .and_then(AgentTool::from_name)
            .filter(|tool| *tool != AgentTool::Finish)
            .ok_or(InvestigationError::InvalidInput)?;
        let context = action.remove("context_id").is_some();
        let index = match groups.iter().position(|(seen, _, _)| *seen == tool) {
            Some(index) => index,
            None => {
                groups.push((tool, Vec::new(), None));
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        if group.2.replace(context).is_some_and(|seen| seen != context) {
            return Err(InvestigationError::InvalidInput);
        }
        group.1.push(Value::Object(action));
    }
    for (tool, actions, context) in groups {
        if tools.iter().any(|existing| existing.name == tool.name()) {
            return Err(InvestigationError::InvalidInput);
        }
        tools.push(ToolDefinition {
            name: tool.name(),
            description: tool.description(),
            input_schema: serde_json::json!({ "type": "object", "enum": actions }),
            host_context_id: context.unwrap_or(false),
        });
    }
    Ok(())
}

/// Removes `type` and `context_id` from an object schema; reports whether
/// `context_id` was present (and so must be injected by the host).
fn strip_host_fields(schema: &mut Value) -> bool {
    let mut had_context = false;
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.remove("type");
        had_context = properties.remove("context_id").is_some();
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|field| field != "type" && field != "context_id");
    }
    had_context
}

/// Turns one native tool call into the bounded loop's exact action bytes, for
/// the existing decoder. Refuses a tool not offered this turn, non-object
/// arguments, and arguments that try to set host-owned fields.
pub fn action_from_tool_call(
    offered: &[ToolDefinition],
    name: &str,
    arguments: &Value,
    context_id: &str,
) -> Result<String, ToolRefusal> {
    let Some(tool) = offered.iter().find(|tool| tool.name == name) else {
        let names: Vec<_> = offered.iter().map(|tool| tool.name).collect();
        return Err(ToolRefusal::new(
            "unknown_tool",
            format!("Use one of the offered tools: {}.", names.join(", ")),
        ));
    };
    let Some(arguments) = arguments.as_object() else {
        return Err(ToolRefusal::new(
            "invalid_arguments",
            "Tool arguments must be a JSON object matching the tool's input schema.",
        ));
    };
    if arguments.contains_key("type") || arguments.contains_key("context_id") {
        return Err(ToolRefusal::new(
            "host_owned_field",
            "Do not send type or context_id; the host supplies them.",
        ));
    }
    let mut action = Map::new();
    action.insert("type".into(), Value::String(name.into()));
    if tool.host_context_id {
        action.insert("context_id".into(), Value::String(context_id.into()));
    }
    action.extend(arguments.clone());
    let action = Value::Object(action);
    let value = if name == AgentTool::Finish.name() {
        let outcome = match action["knowledge_completeness"].as_str() {
            Some("insufficient_evidence") => "insufficient_evidence",
            _ => "supported",
        };
        serde_json::json!({ "schema_version": 1, "outcome": outcome, "action": action })
    } else {
        action
    };
    serde_json::to_string(&value).map_err(|_| {
        ToolRefusal::new(
            "invalid_arguments",
            "Tool arguments could not be serialized.",
        )
    })
}

/// Read-before-finish (AC-1421, D3 of #1382). A supported finish needs at
/// least one admitted evidence read, and every citation it names must have
/// been acquired in this run. An insufficient-evidence finish needs at least
/// one admitted query, so a run cannot give up before looking; reading the
/// coverage inventory alone does not count.
pub fn read_before_finish(
    ledger: &InvestigationInputLedger,
    finish_arguments: &Value,
) -> Result<(), ToolRefusal> {
    let findings = finish_arguments["findings"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if findings.is_empty() {
        if ledger.queries.is_empty() {
            return Err(ToolRefusal::new(
                "read_before_finish",
                "Run query_context before reporting insufficient evidence.",
            ));
        }
        return Ok(());
    }
    if !ledger.citations.iter().any(|c| c.text_hash.is_some()) {
        return Err(ToolRefusal::new(
            "read_before_finish",
            "No source evidence has been read in this run. Call read_evidence on a relevant fact's evidence option before finishing with findings.",
        ));
    }
    let acquired: std::collections::BTreeSet<&str> = ledger
        .citations
        .iter()
        .map(|c| c.citation_id.as_str())
        .collect();
    let mut missing: Vec<&str> = findings
        .iter()
        .filter_map(|finding| finding["citation_ids"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| !acquired.contains(id))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        return Err(ToolRefusal::new(
            "read_before_finish",
            format!(
                "These citation ids were not acquired in this run: {}. Cite only ids returned by query_context or read_evidence, or read the evidence first.",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "native_tools_tests.rs"]
mod tests;
