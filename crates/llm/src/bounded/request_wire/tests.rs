use super::*;
use crate::bounded::{FailureCode, InvocationOutcome, encode};
use serde_json::json;

#[test]
fn ollama_schema_is_identical_in_prompt_and_format() {
    // AC-0454: production serializer only; no endpoint or model execution.
    for template in [
        include_str!("../../../../agents/src/investigation/data/analyst-v13-choice.json"),
        include_str!("../../../../agents/src/investigation/data/analyst-v13-finish-only.json"),
    ] {
        let selected: serde_json::Value = serde_json::from_str(template).unwrap();
        let canonical = selected.to_string();
        let schema = serde_json::value::RawValue::from_string(canonical.clone()).unwrap();
        let prompt = json!({"response_schema": selected}).to_string();
        let user = format!("{prompt}\n\nEvidence spans (JSON):\n[]");
        let bytes = encode(
            &ollama("fixture", "protocol", &user, 2048, Some(&schema)),
            512 * 1024,
            FailureCode::RequestTooLarge,
        )
        .unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let shown = wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .split_once("\n\nEvidence spans (JSON):\n")
            .unwrap()
            .0;
        let shown: serde_json::Value = serde_json::from_str(shown).unwrap();
        assert_eq!(shown["response_schema"], wire["format"]);
        assert_eq!(wire["format"].to_string(), canonical);
    }
}

fn matches_wire_and_cap(value: &impl Serialize, previous: serde_json::Value) {
    let expected = serde_json::to_vec(&previous).unwrap();
    let cap = expected.len() as u64;
    assert_eq!(
        encode(value, cap, FailureCode::RequestTooLarge).unwrap(),
        expected
    );
    for limit in [0, 1, cap - 1] {
        let error = encode(value, limit, FailureCode::RequestTooLarge).unwrap_err();
        assert_eq!(error.code, FailureCode::RequestTooLarge);
        assert_eq!(error.outcome, InvocationOutcome::NotDispatched);
    }
}

#[test]
fn borrowed_ollama_request_preserves_wire_and_exact_cap() {
    // AC-0185: keep exact bounded transport bytes without a request Value clone.
    for text in [String::new(), "café\n\"\\\0".into(), "\0".repeat(4096)] {
        for tokens in [0, 1, u32::MAX] {
            matches_wire_and_cap(
                &ollama(&text, &text, &text, tokens, None),
                json!({
                    "model":text,
                    "messages":[{"role":"system","content":text},{"role":"user","content":text}],
                    "stream":false,"format":"json","think":false,
                    "options":{"num_predict":tokens}
                }),
            );
        }
    }
}

#[test]
fn borrowed_anthropic_request_preserves_wire_and_exact_cap() {
    // AC-0185: refusals remain pre-dispatch at the same encoded-byte boundary.
    for text in [String::new(), "café\n\"\\\0".into(), "\0".repeat(4096)] {
        for tokens in [0, 1, u32::MAX] {
            matches_wire_and_cap(
                &anthropic(&text, &text, &text, tokens),
                json!({
                    "model":text,"max_tokens":tokens,
                    "stream":false,"system":text,
                    "messages":[{"role":"user","content":text}]
                }),
            );
        }
    }
}

#[test]
fn borrowed_openai_compatible_request_preserves_wire_and_exact_cap() {
    // #938: the same encoded-byte boundary for a local OpenAI-compatible server.
    for text in [String::new(), "café\n\"\\\0".into(), "\0".repeat(4096)] {
        for tokens in [0, 1, u32::MAX] {
            matches_wire_and_cap(
                &openai_compatible(&text, &text, &text, tokens),
                json!({
                    "model":text,"max_tokens":tokens,"stream":false,
                    "response_format":{"type":"json_object"},
                    "messages":[{"role":"system","content":text},{"role":"user","content":text}]
                }),
            );
        }
    }
}
