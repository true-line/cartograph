use super::*;
use crate::bounded::{FailureCode, InvocationOutcome, encode};
use serde_json::json;

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
                &ollama(&text, &text, &text, tokens),
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
