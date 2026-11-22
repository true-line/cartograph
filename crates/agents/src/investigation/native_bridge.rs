//! Host side of a tool-calling run's conversation (ADR-0085, AC-1428).
//!
//! The worker keeps producing one authorized bounded payload per step. The
//! bridge turns that sequence into a native tool-calling conversation: the
//! first payload starts the runtime, and each later payload goes back as the
//! result of the model's last tool call, carrying only what changed since the
//! previous authorized payload. Every byte sent is a byte the firewall
//! authorized for that step; the delta only leaves out what the model already
//! has. When the tools, the system text or the prompt shape change, or the
//! conversation would outgrow its budget, the bridge restarts the runtime from
//! the full authorized payload instead.

use llm::{CompletionPayload, PayloadSpan};
use serde_json::{Map, Value};

use super::InvestigationError;
use super::native_tools::{
    ToolDefinition, ToolRefusal, action_from_tool_call, tool_definitions_from_schema,
};

/// What the host sends the runtime for this step.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeTurn {
    /// Start (or restart) the runtime with the full authorized payload.
    Start {
        /// System text.
        system: String,
        /// The full user message, as the bounded transport renders it.
        prompt: String,
        /// The tools offered this step.
        tools: Vec<ToolDefinition>,
        /// Why this is a start, for the run record.
        reason: StartReason,
    },
    /// Answer the model's last tool call with what changed.
    ToolResult {
        /// Tool-result text.
        content: String,
    },
}

/// Why the bridge (re)started the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartReason {
    /// First step of the run.
    First,
    /// The offered tools changed.
    ToolsChanged,
    /// The system text changed.
    SystemChanged,
    /// The prompt is not a JSON object, so no delta is defined.
    PromptNotObject,
    /// The conversation would exceed its byte budget.
    Budget,
}

/// The conversation as the runtime holds it.
#[derive(Debug, Default)]
pub struct BridgeConversation {
    previous: Option<Previous>,
    /// Bytes the runtime's conversation holds: the start message, every tool
    /// result and every model output since the last start.
    conversation_bytes: usize,
}

#[derive(Debug)]
struct Previous {
    system: String,
    prompt: Map<String, Value>,
    spans: Vec<PayloadSpan>,
    tools: Vec<ToolDefinition>,
}

impl BridgeConversation {
    /// A new run's conversation.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes the runtime's conversation holds now.
    pub fn conversation_bytes(&self) -> usize {
        self.conversation_bytes
    }

    /// The tools offered on the last step.
    pub fn offered(&self) -> &[ToolDefinition] {
        self.previous.as_ref().map_or(&[], |p| p.tools.as_slice())
    }

    /// The next turn for `payload`, which the firewall authorized for this
    /// step. `budget_bytes` caps the conversation; past it the runtime starts
    /// over from the full payload, which the bounded loop already sized to fit.
    pub fn next_turn(
        &mut self,
        payload: &CompletionPayload,
        budget_bytes: usize,
    ) -> Result<BridgeTurn, InvestigationError> {
        let schema = payload
            .response_schema
            .as_deref()
            .ok_or(InvestigationError::InvalidInput)?;
        let tools = tool_definitions_from_schema(schema)?;
        let prompt = serde_json::from_str::<Value>(&payload.prompt).ok();
        let reason = match &self.previous {
            None => Some(StartReason::First),
            Some(_) if !matches!(prompt, Some(Value::Object(_))) => {
                Some(StartReason::PromptNotObject)
            }
            Some(previous) if previous.system != payload.system => Some(StartReason::SystemChanged),
            Some(previous) if previous.tools != tools => Some(StartReason::ToolsChanged),
            Some(_) => None,
        };
        let delta = match (reason, &self.previous, &prompt) {
            (None, Some(previous), Some(Value::Object(next))) => {
                Some(tool_result_delta(previous, next, &payload.spans)?)
            }
            _ => None,
        };
        let turn = match (reason, delta) {
            (None, Some(content)) if self.conversation_bytes + content.len() <= budget_bytes => {
                self.conversation_bytes += content.len();
                BridgeTurn::ToolResult { content }
            }
            (reason, _) => {
                let user = full_user_message(payload)?;
                self.conversation_bytes = payload.system.len() + user.len();
                BridgeTurn::Start {
                    system: payload.system.clone(),
                    prompt: user,
                    tools: tools.clone(),
                    reason: reason.unwrap_or(StartReason::Budget),
                }
            }
        };
        self.previous = Some(Previous {
            system: payload.system.clone(),
            prompt: match prompt {
                Some(Value::Object(object)) => object,
                _ => Map::new(),
            },
            spans: payload.spans.clone(),
            tools,
        });
        Ok(turn)
    }

    /// Records the model's output bytes for this step.
    pub fn record_model_output(&mut self, bytes: usize) {
        self.conversation_bytes += bytes;
    }

    /// The model's tool call as the bounded loop's exact action bytes, or the
    /// refusal the runtime returns to the model as a tool error.
    pub fn action_text(&self, name: &str, arguments: &Value) -> Result<String, ToolRefusal> {
        let context_id = self
            .previous
            .as_ref()
            .and_then(|p| p.prompt.get("context_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        action_from_tool_call(self.offered(), name, arguments, context_id)
    }
}

/// The user message exactly as the bounded transport renders it.
fn full_user_message(payload: &CompletionPayload) -> Result<String, InvestigationError> {
    let spans =
        serde_json::to_string(&payload.spans).map_err(|_| InvestigationError::InvalidInput)?;
    Ok(format!(
        "{}\n\nEvidence spans (JSON):\n{}",
        payload.prompt, spans
    ))
}

/// What changed between two authorized payloads: prompt fields set or
/// changed, prompt fields removed, evidence spans added and span ids dropped.
fn tool_result_delta(
    previous: &Previous,
    next: &Map<String, Value>,
    spans: &[PayloadSpan],
) -> Result<String, InvestigationError> {
    let changed: Map<String, Value> = next
        .iter()
        .filter(|(key, value)| previous.prompt.get(*key) != Some(value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let removed: Vec<&String> = previous
        .prompt
        .keys()
        .filter(|key| !next.contains_key(*key))
        .collect();
    let added: Vec<&PayloadSpan> = spans
        .iter()
        .filter(|span| !previous.spans.contains(span))
        .collect();
    let dropped: Vec<&str> = previous
        .spans
        .iter()
        .filter(|span| !spans.iter().any(|s| s.id == span.id))
        .map(|span| span.id.as_str())
        .collect();
    let mut delta = Map::new();
    delta.insert("changed".into(), Value::Object(changed));
    if !removed.is_empty() {
        delta.insert("removed".into(), serde_json::json!(removed));
    }
    if !dropped.is_empty() {
        delta.insert("dropped_spans".into(), serde_json::json!(dropped));
    }
    let mut text = serde_json::to_string(&delta).map_err(|_| InvestigationError::InvalidInput)?;
    if !added.is_empty() {
        let added = serde_json::to_string(&added).map_err(|_| InvestigationError::InvalidInput)?;
        text.push_str("\n\nNew evidence spans (JSON):\n");
        text.push_str(&added);
    }
    Ok(text)
}

#[cfg(test)]
#[path = "native_bridge_tests.rs"]
mod tests;
