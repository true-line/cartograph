//! Logical HTTP payload ownership, not a process or allocator memory ceiling.
use super::response_wire::{
    AnthropicBlock, AnthropicResponse, OllamaResponse, OpenAiChoice, OpenAiResponse,
};
use super::{
    BoundedCompletion, CompletionLimits, MODEL_AND_RESPONSE_ID_BYTES, RECEIVE_CHUNK_BYTES,
};
use std::mem::size_of;

#[derive(Debug, Clone, Copy)]
pub struct TransportWorkspace {
    pub completion_bytes: usize,
    pub request_peak_bytes: usize,
    pub response_peak_bytes: usize,
}

fn sum(values: &[usize]) -> Option<usize> {
    values.iter().try_fold(0usize, |a, b| a.checked_add(*b))
}

impl CompletionLimits {
    /// Payload owners from user_message through response decoding/finish.
    /// Borrowed authorization and provider configuration, profile validation,
    /// HTTP headers/client/TLS internals, stacks and allocator capacity are not
    /// included. Compose those separately; this does not admit a worker.
    /// JSON tree slots use a conservative logical per-value header currency.
    pub fn transport_workspace_bytes(&self) -> Option<TransportWorkspace> {
        let input = usize::try_from(self.0.input_bytes).ok()?;
        let request = usize::try_from(self.0.request_bytes).ok()?;
        let response = usize::try_from(self.0.response_bytes).ok()?;
        let output = usize::try_from(self.0.output_text_bytes).ok()?;
        let user = sum(&[input, input, b"\n\nEvidence spans (JSON):\n".len()])?;
        // Optional schema is borrowed as RawValue from authorized input;
        // transport does not repeat the preparation-only redaction check.
        // Borrowed request_wire DTOs clone no strings or Value tree. The span
        // encoding and formatted user message can coexist before encode caps
        // the wire buffer; charge both phases conservatively together.
        let request_peak_bytes = sum(&[
            input,
            user,
            request,
            2 * size_of::<String>(),
            size_of::<Vec<u8>>(),
        ])?;
        // Semantic checks occur after decode. Invalid block counts or nested
        // tool/image Values can therefore occupy the whole bounded response.
        // Every decoded value/entry needs at least one wire byte. Charge text
        // once plus one Value/String/tree-link slot per possible value. This
        // is logical payload, not BTreeMap implementation/allocator storage.
        let json_slot = sum(&[
            size_of::<serde_json::Value>(),
            size_of::<String>(),
            3 * size_of::<usize>(),
        ])?;
        let nested_values = sum(&[response, response.checked_add(1)?.checked_mul(json_slot)?])?;
        let typed = sum(&[
            size_of::<AnthropicResponse>()
                .max(size_of::<OllamaResponse>())
                .max(size_of::<OpenAiResponse>()),
            response,
            // Anthropic content blocks or OpenAI-compatible choices.
            response
                .checked_add(1)?
                .checked_mul(size_of::<AnthropicBlock>().max(size_of::<OpenAiChoice>()))?,
            nested_values,
        ])?;
        // Profile validation and finish share the identifier cap.
        let completion_bytes = sum(&[
            size_of::<BoundedCompletion>(),
            output,
            3 * MODEL_AND_RESPONSE_ID_BYTES,
        ])?;
        let response_peak_bytes = sum(&[
            user,
            request,
            response,
            RECEIVE_CHUNK_BYTES,
            typed,
            response, // decoder escape scratch; includes rejected fields
            output,   // Anthropic concatenation while decoded blocks still live
            completion_bytes,
            3 * size_of::<Vec<u8>>(),
            size_of::<String>(),
        ])?;
        Some(TransportWorkspace {
            completion_bytes,
            request_peak_bytes,
            response_peak_bytes,
        })
    }
}

#[cfg(test)]
#[test]
fn transport_workspace_rejects_unrepresentable_components() {
    // AC-0327: internal invalid limits exercise checked arithmetic, not admission.
    let limits = CompletionLimits(super::CompletionLimitValues {
        response_bytes: u64::MAX,
        ..Default::default()
    });
    assert!(limits.transport_workspace_bytes().is_none());
}
