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
                &ollama(&text, &text, &text, tokens, None, false, None),
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

// #1653: a chosen context window is the only addition to the request bytes.
#[test]
fn ollama_context_window_is_sent_only_when_chosen() {
    matches_wire_and_cap(
        &ollama("m", "s", "u", 7, None, false, Some(32768)),
        json!({
            "model":"m",
            "messages":[{"role":"system","content":"s"},{"role":"user","content":"u"}],
            "stream":false,"format":"json","think":false,
            "options":{"num_ctx":32768,"num_predict":7}
        }),
    );
}

#[test]
fn borrowed_anthropic_request_preserves_wire_and_exact_cap() {
    // AC-0185: refusals remain pre-dispatch at the same encoded-byte boundary.
    for text in [String::new(), "café\n\"\\\0".into(), "\0".repeat(4096)] {
        for tokens in [0, 1, u32::MAX] {
            matches_wire_and_cap(
                &anthropic(&text, &text, &text, tokens, None),
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
fn anthropic_schema_request_uses_output_config() {
    // AC-0568 / #1709: structured output travels as output_config.format; no beta header.
    let schema =
        json!({"type":"object","additionalProperties":false,"required":[],"properties":{}});
    matches_wire_and_cap(
        &anthropic("m", "s", "u", 7, Some(&schema)),
        json!({
            "model":"m","max_tokens":7,"stream":false,"system":"s",
            "messages":[{"role":"user","content":"u"}],
            "output_config":{"format":{"type":"json_schema","schema":schema}}
        }),
    );
}

#[test]
fn borrowed_openai_compatible_request_preserves_wire_and_exact_cap() {
    // #938: the same encoded-byte boundary for a local OpenAI-compatible server.
    for text in [String::new(), "café\n\"\\\0".into(), "\0".repeat(4096)] {
        for tokens in [0, 1, u32::MAX] {
            matches_wire_and_cap(
                &openai_compatible(&text, &text, &text, tokens, None, None),
                json!({
                    "model":text,"max_tokens":tokens,"stream":false,
                    "response_format":{"type":"json_object"},
                    "messages":[{"role":"system","content":text},{"role":"user","content":text}]
                }),
            );
        }
    }
}

#[test]
fn openai_schema_and_template_fields_count_toward_exact_request_cap() {
    let schema = serde_json::value::RawValue::from_string(
        r#"{"type":"object","dependencies":{"x":["uniqueItems"],"y":{"type":"array","uniqueItems":true}},"properties":{"uniqueItems":{"const":{"uniqueItems":true}}},"$defs":{"z":{"anyOf":[{"uniqueItems":true,"type":"array"}]},"a":{"type":"array","uniqueItems":false}}}"#.into(),
    ).unwrap();
    let wire = openai_compatible("model", "system", "user", 37, Some(&schema), Some(false));
    let expected = br#"{"chat_template_kwargs":{"enable_thinking":false},"max_tokens":37,"messages":[{"content":"system","role":"system"},{"content":"user","role":"user"}],"model":"model","response_format":{"type":"json_schema","json_schema":{"name":"cartograph_bounded_response","strict":true,"schema":{"type":"object","dependencies":{"x":["uniqueItems"],"y":{"type":"array"}},"properties":{"uniqueItems":{"const":{"uniqueItems":true}}},"$defs":{"z":{"anyOf":[{"type":"array"}]},"a":{"type":"array"}}}}},"stream":false}"#;
    let encode_at = |limit| {
        crate::bounded::encode_with_formatter(
            &wire,
            limit,
            FailureCode::RequestTooLarge,
            OpenAiSchemaFormatter,
        )
    };
    assert_eq!(encode_at(expected.len() as u64).unwrap(), expected);
    let error = encode_at(expected.len() as u64 - 1).unwrap_err();
    assert_eq!(error.code, FailureCode::RequestTooLarge);
    assert_eq!(error.outcome, InvocationOutcome::NotDispatched);
}
