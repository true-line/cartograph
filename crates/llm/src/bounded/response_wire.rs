//! Production response DTOs shared with logical payload accounting.
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct OllamaMessage {
    pub(super) role: String,
    pub(super) content: String,
    #[serde(default)]
    pub(super) tool_calls: Vec<serde_json::Value>,
    #[serde(default)]
    pub(super) images: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
pub(super) struct OllamaResponse {
    pub(super) message: OllamaMessage,
    pub(super) done: bool,
    pub(super) done_reason: String,
    pub(super) model: Option<String>,
    pub(super) prompt_eval_count: Option<u64>,
    pub(super) eval_count: Option<u64>,
}

#[derive(Deserialize)]
pub(super) struct AnthropicBlock {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) text: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct AnthropicStopDetails {
    #[serde(rename = "type")]
    pub(super) kind: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct AnthropicUsage {
    pub(super) input_tokens: Option<u64>,
    pub(super) output_tokens: Option<u64>,
    pub(super) cache_read_input_tokens: Option<u64>,
    pub(super) cache_creation_input_tokens: Option<u64>,
}
#[derive(Deserialize)]
pub(super) struct AnthropicResponse {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) role: String,
    pub(super) content: Vec<AnthropicBlock>,
    pub(super) stop_reason: String,
    pub(super) stop_details: Option<AnthropicStopDetails>,
    pub(super) model: Option<String>,
    pub(super) id: Option<String>,
    pub(super) usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
pub(super) struct OpenAiMessage {
    pub(super) role: String,
    pub(super) content: Option<String>,
    pub(super) refusal: Option<String>,
    pub(super) tool_calls: Option<Vec<serde_json::Value>>,
    pub(super) function_call: Option<serde_json::Value>,
}
#[derive(Deserialize)]
pub(super) struct OpenAiChoice {
    pub(super) message: OpenAiMessage,
    pub(super) finish_reason: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct OpenAiUsage {
    pub(super) prompt_tokens: Option<u64>,
    pub(super) completion_tokens: Option<u64>,
}
#[derive(Deserialize)]
pub(super) struct OpenAiResponse {
    pub(super) object: String,
    pub(super) choices: Vec<OpenAiChoice>,
    pub(super) model: Option<String>,
    pub(super) id: Option<String>,
    pub(super) usage: Option<OpenAiUsage>,
}
