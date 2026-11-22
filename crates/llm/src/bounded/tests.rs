use super::*;
use crate::anthropic::{AnthropicProvider, ClaudeLane};
use crate::openai_compatible::OpenAiCompatibleProvider;
use crate::{AnalysisTier, EgressPolicy, OllamaProvider, PayloadSpan, ProviderError};
use serde_json::{Value, json};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};

const RESPONSE_SCHEMA: &str = r#"{"anyOf":[{"type":"object","properties":{"type":{"enum":["query_context","read_evidence"]}},"required":["type"],"additionalProperties":false},{"type":"object","properties":{"schema_version":{"const":1},"outcome":{"enum":["supported","insufficient_evidence"]},"action":{"type":"object"}},"required":["schema_version","outcome","action"],"additionalProperties":false}]}"#;

struct FramedRequest {
    headers: String,
    body: Vec<u8>,
}

// Read the actual complete HTTP request, not whatever one TCP read happens to
// return. Fixtures reject unexpected framing and cap both headers and body.
fn read_request(stream: &mut impl Read) -> io::Result<FramedRequest> {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() == 16 * 1024 {
            return Err(io::Error::other("fixture headers too large"));
        }
        let mut byte = [0_u8];
        stream.read_exact(&mut byte)?;
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).map_err(io::Error::other)?;
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>())
        })
        .ok_or_else(|| io::Error::other("fixture missing content length"))?
        .map_err(io::Error::other)?;
    if length > 512 * 1024 {
        return Err(io::Error::other("fixture request too large"));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok(FramedRequest { headers, body })
}

fn set_timeouts(stream: &TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
}

/// A loopback endpoint that refuses connections and stays reserved while the
/// returned socket lives. Dropping a listener to get a closed port let a
/// parallel test bind the same port and receive this test's probe.
pub(crate) fn refused_endpoint() -> (socket2::Socket, SocketAddr) {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    socket
        .bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .unwrap();
    let address = socket.local_addr().unwrap().as_socket().unwrap();
    (socket, address)
}

// Reuse the bounded complete-frame reader for a test-only EOF transport fixture.
pub(crate) fn consume_request(stream: &mut TcpStream) -> io::Result<()> {
    set_timeouts(stream);
    read_request(stream).map(|_| ())
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

fn http(response: Vec<u8>) -> (String, JoinHandle<FramedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        set_timeouts(&stream);
        let request = read_request(&mut stream).unwrap();
        // Limit failures intentionally close the socket before its full body.
        let _ = stream.write_all(&response);
        request
    });
    (format!("http://{address}/"), server)
}

fn ollama_body(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model":"test-model", "message":{"role":"assistant", "content":text},
        "done":true, "done_reason":"stop", "prompt_eval_count":11, "eval_count":7,
    }))
    .unwrap()
}

fn anthropic_body(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type":"message", "role":"assistant", "model":ClaudeLane::Opus.model_id(), "id":"msg_fixture",
        "content":[{"type":"text", "text":text}], "stop_reason":"end_turn",
        "usage":{"input_tokens":23,"output_tokens":7,"cache_read_input_tokens":2,
            "cache_creation_input_tokens":3,"service_tier":"standard"}
    })).unwrap()
}

fn action() -> CompletionAction {
    CompletionAction {
        action_id: "investigation-1:step-1".into(),
        tier: AnalysisTier::Agentic,
        payload: CompletionPayload {
            response_schema: None,
            system: "Return exactly one JSON action.".into(),
            prompt: "Investigate the selected rules.".into(),
            spans: vec![PayloadSpan {
                id: "evidence-1".into(),
                repo: "src_fixture".into(),
                path: "src/rules.ts".into(),
                byte_start: 10,
                byte_end: 40,
                commit_sha: "captured".into(),
                text: "if (blocked) return false;".into(),
            }],
        },
    }
}

#[test]
fn prepared_bounded_action_binding_survives_redaction_without_accepting_replacements() {
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let mut original = action();
    original.payload.spans[0].text = "password=supersecret123".into();
    let prepared = firewall
        .prepare_bounded(&provider, &original, &CompletionLimits::default())
        .unwrap();
    assert!(prepared.matches_action(&original));
    assert_ne!(
        prepared.preview().egress.payload.spans[0].text,
        original.payload.spans[0].text
    );
    let mut replacement = original.clone();
    replacement.payload.spans[0].text = "password=differentsecret456".into();
    assert!(!prepared.matches_action(&replacement));
    replacement = original.clone();
    replacement.action_id = "different-action".into();
    assert!(!prepared.matches_action(&replacement));
    replacement = original;
    replacement.payload.spans[0].path = "different.ts".into();
    assert!(!prepared.matches_action(&replacement));
}

fn authorize(provider: &dyn LlmProvider, limits: CompletionLimits) -> AuthorizedBoundedCompletion {
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let prepared = firewall
        .prepare_bounded(provider, &action(), &limits)
        .unwrap();
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    firewall
        .authorize_bounded(provider, &prepared, Some(&grant))
        .unwrap()
}

fn proceed() -> CallDirective {
    CallDirective::Continue
}
fn control() -> CompletionControl<'static> {
    CompletionControl {
        deadline: Instant::now() + Duration::from_secs(10),
        check: &proceed,
        dispatching: None,
    }
}

fn assert_no_request(listener: &TcpListener) {
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener
            .accept()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
    );
}

// #1653: a provider with a chosen context window sends it as `num_ctx`.
#[test]
fn ollama_sends_the_chosen_context_window() {
    let limits = CompletionLimits::new(CompletionLimitValues::default()).unwrap();
    let local_body = ollama_body(r#"{"action":"query_context"}"#);
    let (endpoint, server) = http(framed(&local_body));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10))
        .unwrap()
        .with_context_window(16384)
        .unwrap();
    local
        .complete_bounded(&authorize(&local, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["options"]["num_ctx"], 16384);
    assert_eq!(wire["options"]["num_predict"], 2048);
}

// AC-0185: actual provider requests carry finite generation limits; complete
// response metadata remains distinct from caller reservations and body measures.
#[test]
fn bounded_providers_send_exact_generation_limits_and_consented_payload() {
    let values = CompletionLimitValues {
        max_output_tokens: 37,
        ..Default::default()
    };
    let limits = CompletionLimits::new(values).unwrap();
    let local_body = ollama_body(r#"{"action":"query_context"}"#);
    let (endpoint, server) = http(framed(&local_body));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let output = local
        .complete_bounded(&authorize(&local, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["options"]["num_predict"], 37);
    assert!(wire["options"].get("num_ctx").is_none());
    assert_eq!(wire["format"], "json");
    assert_eq!(wire["think"], false);
    assert_eq!(
        local.bounded_profile().unwrap().protocol_version,
        "bounded-json-completion@2"
    );
    assert_eq!(wire["stream"], false);
    assert!(
        wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("if (blocked) return false;")
    );
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("accept-encoding: identity")
    );
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, local_body.len() as u64);
    let workspace = limits.transport_workspace_bytes().unwrap();
    assert!(request.body.len() <= workspace.request_peak_bytes);
    assert!(local_body.len() <= workspace.response_peak_bytes);
    assert!(serde_json::to_vec(&output).unwrap().len() <= workspace.completion_bytes);
    assert_eq!(output.usage.unwrap().output_tokens, Some(7));
    assert_eq!(output.response_model.as_deref(), Some("test-model"));

    // Same real bounded dispatch with schema object framing, no model involved.
    let (endpoint, server) = http(framed(&local_body));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let mut selected = action();
    selected.payload.response_schema = Some(RESPONSE_SCHEMA.into());
    let prepared = firewall
        .prepare_bounded(&local, &selected, &limits)
        .unwrap();
    let authorized = firewall.authorize_bounded(&local, &prepared, None).unwrap();
    let output = local.complete_bounded(&authorized, &control()).unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        wire["format"],
        serde_json::from_str::<Value>(RESPONSE_SCHEMA).unwrap()
    );
    assert_eq!(wire["options"]["num_predict"], 37);
    assert_eq!(wire["think"], false);
    assert_eq!(output.request_bytes, request.body.len() as u64);

    let cloud_body = anthropic_body(r#"{"action":"finish"}"#);
    let (endpoint, server) = http(framed(&cloud_body));
    let cloud =
        AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint).unwrap();
    let output = cloud
        .complete_bounded(&authorize(&cloud, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["max_tokens"], 37);
    assert_eq!(wire["model"], ClaudeLane::Opus.model_id());
    assert!(wire.get("fallbacks").is_none());
    assert!(wire.get("tools").is_none());
    assert!(wire.get("think").is_none());
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("anthropic-version: 2023-06-01")
    );
    assert_eq!(output.provider_request_id.as_deref(), Some("msg_fixture"));
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, cloud_body.len() as u64);
    assert!(request.body.len() <= workspace.request_peak_bytes);
    assert!(cloud_body.len() <= workspace.response_peak_bytes);
    assert!(serde_json::to_vec(&output).unwrap().len() <= workspace.completion_bytes);
    let usage = output.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(23));
    assert_eq!(usage.cache_read_input_tokens, Some(2));
}

// AC-0185: cap the entire response entity, including ignored fields and chunked
// data, before JSON. Test exact boundary and multibyte content too.
#[test]
fn bounded_response_reader_enforces_body_cap_before_decode() {
    let valid = ollama_body("évidence");
    let cap = valid.len() as u64;
    let mut ignored: Value = serde_json::from_slice(&valid).unwrap();
    ignored["ignored"] = json!("x".repeat(4096));
    let large = serde_json::to_vec(&ignored).unwrap();
    let mut chunked =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    chunked.extend_from_slice(format!("{:x}\r\n", large.len()).as_bytes());
    chunked.extend_from_slice(&large);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    let mut no_length = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    no_length.extend_from_slice(&large);
    let declared = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        cap + 1
    )
    .into_bytes();
    for response in [framed(&large), chunked, no_length, declared] {
        let (endpoint, server) = http(response);
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let limits = CompletionLimits::new(CompletionLimitValues {
            response_bytes: cap,
            ..Default::default()
        })
        .unwrap();
        let error = local
            .complete_bounded(&authorize(&local, limits), &control())
            .unwrap_err();
        assert_eq!(error, rejected(FailureCode::ResponseTooLarge));
        server.join().unwrap();
    }
    let (endpoint, server) = http(framed(&valid));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let limits = CompletionLimits::new(CompletionLimitValues {
        response_bytes: cap,
        output_text_bytes: "évidence".len() as u64,
        ..Default::default()
    })
    .unwrap();
    let output = local
        .complete_bounded(&authorize(&local, limits), &control())
        .unwrap();
    assert_eq!(output.text, "évidence");
    assert_eq!(output.response_bytes, cap);
    server.join().unwrap();
}

// AC-0185: compression cannot turn a capped encoded body into uncapped decoded
// memory, even if another workspace crate enables a Reqwest compression feature.
#[test]
fn bounded_transport_rejects_content_encoding_and_oversized_action_text() {
    let mut encoded = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\nConnection: close\r\n\r\n".to_vec();
    encoded.extend_from_slice(b"{}");
    let (endpoint, server) = http(encoded);
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let error = local
        .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
        .unwrap_err();
    assert_eq!(error, rejected(FailureCode::UnsupportedEncoding));
    server.join().unwrap();
    let (endpoint, server) = http(framed(&ollama_body("éé")));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let limits = CompletionLimits::new(CompletionLimitValues {
        output_text_bytes: 3,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, limits), &control())
            .unwrap_err(),
        rejected(FailureCode::OutputTooLarge)
    );
    server.join().unwrap();
}

// AC-0185: a syntactically valid JSON action is still unusable if the provider
// reports truncation/refusal, a native tool, a malformed envelope or unknown stop.
// A stop at the output limit is reported distinctly as OutputLimitReached.
#[test]
fn bounded_completion_rejects_truncated_refused_and_unknown_envelopes() {
    let mut local: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    let mut cloud: Value = serde_json::from_slice(&anthropic_body("{}")).unwrap();
    let mut cases = Vec::new();
    local["done_reason"] = json!("length");
    cases.push((false, local.clone(), FailureCode::OutputLimitReached));
    // Precedence: an unfinished stream never claims the output limit.
    local["done"] = json!(false);
    cases.push((false, local.clone(), FailureCode::IncompleteResponse));
    local["done"] = json!(true);
    local["done_reason"] = json!("unexpected");
    cases.push((false, local.clone(), FailureCode::IncompleteResponse));
    local["done_reason"] = json!("stop");
    local["message"]["tool_calls"] = json!([{"function":{"name":"shell"}}]);
    cases.push((false, local, FailureCode::InvalidResponse));
    cloud["stop_reason"] = json!("max_tokens");
    cases.push((true, cloud.clone(), FailureCode::OutputLimitReached));
    // Precedence: a declared refusal wins over the output limit.
    cloud["stop_details"] = json!({"type":"refusal","explanation":"PRIVATE SOURCE"});
    cases.push((true, cloud.clone(), FailureCode::Refused));
    cloud["stop_details"] = Value::Null;
    // Any other non-success stop stays Incomplete (AC-0570 diagnostic only).
    cloud["stop_reason"] = json!("pause_turn");
    cases.push((true, cloud.clone(), FailureCode::IncompleteResponse));
    cloud["stop_reason"] = json!("end_turn");
    cloud["stop_details"] = json!({"type":"refusal","explanation":"PRIVATE SOURCE"});
    cases.push((true, cloud.clone(), FailureCode::Refused));
    cloud["stop_details"] = Value::Null;
    cloud["content"] =
        json!([{"type":"tool_use","name":"shell","input":{"command":"PRIVATE SOURCE"}}]);
    cases.push((true, cloud, FailureCode::InvalidResponse));
    cases.push((
        false,
        json!({"message":{"content":"{}"}}),
        FailureCode::InvalidResponse,
    ));
    for (cloud, body, expected) in cases {
        let (endpoint, server) = http(framed(&serde_json::to_vec(&body).unwrap()));
        let provider: Box<dyn LlmProvider> = if cloud {
            Box::new(
                AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint)
                    .unwrap(),
            )
        } else {
            Box::new(OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap())
        };
        let error = provider
            .complete_bounded(
                &authorize(provider.as_ref(), CompletionLimits::default()),
                &control(),
            )
            .unwrap_err();
        assert_eq!(error, rejected(expected));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
}

// AC-0185: ignored JSON is still subject to a structural preflight, invalid UTF-8
// and impossible reported usage fail closed, and absent token counts stay absent.
#[test]
fn bounded_provider_json_preflight_and_usage_are_explicit() {
    let mut nested = ollama_body("{}");
    nested.pop();
    nested.extend_from_slice(b",\"ignored\":");
    nested.extend_from_slice("[".repeat(65).as_bytes());
    nested.push(b'0');
    nested.extend_from_slice("]".repeat(65).as_bytes());
    nested.push(b'}');
    let mut negative: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    negative["eval_count"] = json!(-1);
    let mut excess = negative.clone();
    excess["eval_count"] = json!(2049);
    for body in [
        nested,
        serde_json::to_vec(&negative).unwrap(),
        serde_json::to_vec(&excess).unwrap(),
        b"PRIVATE SOURCE \xff".to_vec(),
    ] {
        let (endpoint, server) = http(framed(&body));
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let error = local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap_err();
        assert_eq!(error, rejected(FailureCode::InvalidResponse));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
    let mut missing: Value = serde_json::from_slice(&ollama_body("{}")).unwrap();
    missing.as_object_mut().unwrap().remove("eval_count");
    missing.as_object_mut().unwrap().remove("prompt_eval_count");
    let (endpoint, server) = http(framed(&serde_json::to_vec(&missing).unwrap()));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    assert!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap()
            .usage
            .is_none()
    );
    server.join().unwrap();
}

// AC-0185: a single total timeout covers headers plus body; receiving an early
// body fragment must not restart the deadline. The server barrier controls stalls.
#[test]
fn bounded_completion_deadline_covers_headers_and_body() {
    for headers_first in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let (release, stalled) = mpsc::channel();
        let (seen, observed) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            set_timeouts(&stream);
            read_request(&mut stream).unwrap();
            if headers_first {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                    .unwrap();
            }
            seen.send(()).unwrap();
            if headers_first {
                // Keep each read active well inside the request timeout. A
                // timeout which restarts for each fragment cannot pass this case.
                for _ in 0..30 {
                    match stalled.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if stream.write_all(b" ").is_err() {
                                break;
                            }
                        }
                    }
                }
            } else {
                stalled.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        // Remaining host budget is shorter than the provider's declared cap.
        let request = authorize(&local, CompletionLimits::default());
        let before = Instant::now();
        let error = local
            .complete_bounded(
                &request,
                &CompletionControl {
                    deadline: before + Duration::from_millis(300),
                    check: &proceed,
                    dispatching: None,
                },
            )
            .unwrap_err();
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(error, unknown(FailureCode::DeadlineExceeded));
        assert!(before.elapsed() < Duration::from_secs(3));
        let _ = release.send(());
        server.join().unwrap();
    }
}

// AC-0580 / #1767: the host learns the exact request-body size before the
// transport is used, so an egress is counted even when the call then fails.
#[test]
fn bounded_dispatch_hook_reports_request_bytes_before_a_failed_call() {
    let (endpoint, server) = http(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec(),
    );
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let request = authorize(&local, CompletionLimits::default());
    let reported = std::cell::Cell::new(None);
    let dispatching = |bytes: u64| {
        assert_eq!(reported.replace(Some(bytes)), None, "reported once");
        true
    };
    let error = local
        .complete_bounded(
            &request,
            &CompletionControl {
                deadline: Instant::now() + Duration::from_secs(5),
                check: &proceed,
                dispatching: Some(&dispatching),
            },
        )
        .unwrap_err();
    assert_eq!(error, rejected(FailureCode::HttpStatus));
    let sent = server.join().unwrap();
    assert_eq!(reported.get(), Some(sent.body.len() as u64));

    // A successful call reports the same count its completion carries.
    let (endpoint, server) = http(framed(&ollama_body("{}")));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let request = authorize(&local, CompletionLimits::default());
    reported.set(None);
    let completion = local
        .complete_bounded(
            &request,
            &CompletionControl {
                deadline: Instant::now() + Duration::from_secs(5),
                check: &proceed,
                dispatching: Some(&dispatching),
            },
        )
        .unwrap();
    server.join().unwrap();
    assert_eq!(reported.get(), Some(completion.request_bytes));
}

// AC-0580 / #1767: a host that cannot record the egress does not perform it.
#[test]
fn bounded_dispatch_hook_refusal_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let request = authorize(&local, CompletionLimits::default());
    let error = local
        .complete_bounded(
            &request,
            &CompletionControl {
                deadline: Instant::now() + Duration::from_secs(5),
                check: &proceed,
                dispatching: Some(&|_| false),
            },
        )
        .unwrap_err();
    assert_eq!(error, not_sent(FailureCode::EgressDenied));
    assert_no_request(&listener);
}

// AC-0186/0187: pre-dispatch cancellation has no request. After dispatch the real
// worker waits for a bounded outcome; cancellation cannot erase a complete finish.
#[test]
fn bounded_completion_cancellation_preserves_complete_late_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let request = authorize(&local, CompletionLimits::default());
    for directive in [CallDirective::Cancelled, CallDirective::OwnershipLost] {
        let expected = if directive == CallDirective::Cancelled {
            FailureCode::Cancelled
        } else {
            FailureCode::OwnershipLost
        };
        let error = local
            .complete_bounded(
                &request,
                &CompletionControl {
                    deadline: Instant::now() + Duration::from_secs(5),
                    check: &|| directive,
                    dispatching: None,
                },
            )
            .unwrap_err();
        assert_eq!(error, not_sent(expected));
        assert_no_request(&listener);
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let cancelled = Arc::new(AtomicBool::new(false));
    let server_cancelled = Arc::clone(&cancelled);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        set_timeouts(&stream);
        read_request(&mut stream).unwrap();
        server_cancelled.store(true, Ordering::SeqCst);
        stream
            .write_all(&framed(&ollama_body(r#"{"action":"finish"}"#)))
            .unwrap();
    });
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let request = authorize(&local, CompletionLimits::default());
    let check = || {
        if cancelled.load(Ordering::SeqCst) {
            CallDirective::Cancelled
        } else {
            CallDirective::Continue
        }
    };
    let control = CompletionControl {
        deadline: Instant::now() + Duration::from_secs(5),
        check: &check,
        dispatching: None,
    };
    let result = local.complete_bounded(&request, &control).unwrap();
    assert_eq!(result.text, r#"{"action":"finish"}"#);
    assert_eq!(
        local.complete_bounded(&request, &control).unwrap_err(),
        not_sent(FailureCode::Cancelled)
    );
    server.join().unwrap();
}

// AC-0185/0186: the new hash includes profile, endpoint, action, payload and
// limits. Legacy hashes stay on their old recipe; cloud admission remains closed.
#[test]
fn bounded_consent_binds_payload_model_endpoint_and_limits_without_changing_legacy() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let cloud = AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint.clone())
        .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let prepared = firewall
        .prepare_bounded(&cloud, &action(), &CompletionLimits::default())
        .unwrap();
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    let legacy = firewall.preview(&cloud, &action()).unwrap();
    let old_bytes = serde_json::to_vec(&(
        cloud.id(),
        cloud.locality(),
        action().tier,
        action().action_id,
        legacy.payload.clone(),
    ))
    .unwrap();
    assert_eq!(
        legacy.payload_hash,
        blake3::hash(&old_bytes).to_hex().to_string()
    );
    assert_ne!(legacy.payload_hash, prepared.preview().egress.payload_hash);
    let old_grant = ConsentGrant::from_preview(&legacy);
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &prepared, Some(&old_grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let denied = EgressFirewall::new(EgressPolicy::local_only());
    assert_eq!(
        denied
            .authorize_bounded(&cloud, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::EgressDenied)
    );
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &prepared, None)
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentRequired)
    );
    let changed_limits = CompletionLimits::new(CompletionLimitValues {
        max_output_tokens: 100,
        ..Default::default()
    })
    .unwrap();
    let changed = firewall
        .prepare_bounded(&cloud, &action(), &changed_limits)
        .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let mut next = action();
    next.action_id = "investigation-1:step-2".into();
    next.payload.prompt.push_str(" New tool result.");
    let changed = firewall
        .prepare_bounded(&cloud, &next, &CompletionLimits::default())
        .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let other =
        AnthropicProvider::with_endpoint(ClaudeLane::Haiku, "fixture-key", endpoint).unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&other, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ProfileChanged)
    );
    let other = AnthropicProvider::with_endpoint(
        ClaudeLane::Opus,
        "fixture-key",
        "http://127.0.0.1:1/".into(),
    )
    .unwrap();
    assert_eq!(
        firewall
            .authorize_bounded(&other, &prepared, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ProfileChanged)
    );
    assert_no_request(&listener);
}

// AC-0185: invalid or oversized caller input is refused before dispatch, bounded
// serialization covers escaping expansion, and source metadata is redacted too.
#[test]
fn bounded_input_admission_and_serialization_fail_before_dispatch() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let mut input = action();
    input.payload.prompt = "x".repeat(128 * 1024 + 1);
    assert_eq!(
        firewall
            .prepare_bounded(&local, &input, &CompletionLimits::default())
            .err()
            .unwrap(),
        not_sent(FailureCode::InputTooLarge)
    );
    let input = action();
    let limits = CompletionLimits::new(CompletionLimitValues {
        request_bytes: 16,
        ..Default::default()
    })
    .unwrap();
    let prepared = firewall.prepare_bounded(&local, &input, &limits).unwrap();
    let authorized = firewall.authorize_bounded(&local, &prepared, None).unwrap();
    assert_eq!(
        local.complete_bounded(&authorized, &control()).unwrap_err(),
        not_sent(FailureCode::RequestTooLarge)
    );
    let mut input = action();
    input.payload.spans.push(input.payload.spans[0].clone());
    assert_eq!(
        firewall
            .prepare_bounded(&local, &input, &CompletionLimits::default())
            .err()
            .unwrap(),
        not_sent(FailureCode::InvalidInput)
    );
    let mut input = action();
    input.payload.spans[0].repo = "password=super-secret".into();
    input.payload.prompt.push_str(" access_token=super-secret");
    let prepared = firewall
        .prepare_bounded(&local, &input, &CompletionLimits::default())
        .unwrap();
    assert!(
        !serde_json::to_string(prepared.preview())
            .unwrap()
            .contains("super-secret")
    );
    assert_no_request(&listener);
    let invalid = CompletionLimitValues {
        response_bytes: 0,
        ..Default::default()
    };
    assert_eq!(
        CompletionLimits::new(invalid).unwrap_err(),
        not_sent(FailureCode::InvalidLimits)
    );
    assert!(
        serde_json::from_value::<CompletionLimits>(serde_json::to_value(invalid).unwrap()).is_err()
    );
    let over = CompletionLimitValues {
        max_output_tokens: 2049,
        ..Default::default()
    };
    assert!(CompletionLimits::new(over).is_err());
}

// AC-0185: configured no-redirect/no-retry behavior is exercised against actual
// sockets; errors disclose neither Location nor a server-controlled error body.
#[test]
fn bounded_transport_never_follows_redirects_or_retries() {
    let destination = TcpListener::bind("127.0.0.1:0").unwrap();
    let location = format!(
        "http://{}/private-source",
        destination.local_addr().unwrap()
    );
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
    )
    .into_bytes();
    let (endpoint, server) = http(response);
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap_err(),
        rejected(FailureCode::HttpStatus)
    );
    server.join().unwrap();
    assert_no_request(&destination);

    for response in [Some(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 14\r\nConnection: close\r\n\r\nPRIVATE SOURCE".to_vec()), None] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap(); set_timeouts(&stream);
            read_request(&mut stream).unwrap();
            if let Some(body) = response { stream.write_all(&body).unwrap(); }
            drop(stream);
            listener
        });
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let limits = CompletionLimits::new(CompletionLimitValues { request_timeout_ms: 250, ..Default::default() }).unwrap();
        let error = local.complete_bounded(&authorize(&local, limits), &control()).unwrap_err();
        assert!(matches!(error.code, FailureCode::HttpStatus | FailureCode::Transport));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        assert_no_request(&server.join().unwrap());
    }
}

// AC-0185/0186: even a caller-added proxy cannot redirect a declared local
// bounded invocation off its direct loopback path.
#[test]
fn bounded_local_transport_ignores_caller_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let (endpoint, server) = http(framed(&ollama_body("{}")));
    let mut local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    local.bounded_client = build_client(
        Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.local_addr().unwrap())).unwrap()),
        true,
    )
    .unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, CompletionLimits::default()), &control())
            .unwrap()
            .text,
        "{}"
    );
    server.join().unwrap();
    assert_no_request(&proxy);
}

// AC-0185: adding the bounded SPI cannot accidentally invoke a legacy-only
// implementation, even if its unbounded completion is otherwise supported.
#[test]
fn bounded_provider_spi_never_falls_back_to_legacy_complete() {
    struct Legacy(AtomicUsize);
    impl LlmProvider for Legacy {
        fn id(&self) -> &str {
            "legacy"
        }
        fn locality(&self) -> Locality {
            Locality::Local
        }
        fn capabilities(&self) -> crate::ProviderCaps {
            crate::ProviderCaps {
                embeddings: false,
                chat: true,
                tool_use: false,
            }
        }
        fn embed(&self, _: &[String]) -> Result<Vec<crate::Embedding>, crate::ProviderError> {
            Err(crate::ProviderError::Unsupported("embed"))
        }
        fn complete(
            &self,
            _: &crate::ProviderCompletionRequest,
        ) -> Result<crate::Completion, crate::ProviderError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(crate::Completion { text: "{}".into() })
        }
    }
    let provider = Legacy(AtomicUsize::new(0));
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    assert_eq!(
        firewall
            .prepare_bounded(&provider, &action(), &CompletionLimits::default())
            .err()
            .unwrap(),
        BoundedCallError::unsupported()
    );
    assert_eq!(provider.0.load(Ordering::SeqCst), 0);
}

fn https() -> (SocketAddr, JoinHandle<Option<FramedRequest>>) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let cert = CertificateDer::from(include_bytes!("fixtures/server.der").to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        include_bytes!("fixtures/server-key.der").to_vec(),
    ));
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        set_timeouts(&socket);
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let request = read_request(&mut stream).ok()?;
        stream.write_all(&framed(&anthropic_body("{}"))).unwrap();
        stream.flush().unwrap();
        Some(request)
    });
    (address, server)
}

// AC-0185: this is a real TLS handshake and encrypted HTTP exchange with the
// actual Anthropic bounded method. Trust is test-local; production validation
// remains enabled. HTTP-only mocks cannot establish this requirement.
#[test]
fn bounded_cloud_transport_requires_verified_tls_and_hostname() {
    for (trust, hostname) in [
        (true, "localhost"),
        (false, "localhost"),
        (true, "wrong.invalid"),
    ] {
        let (address, server) = https();
        let mut builder = Client::builder()
            .tls_backend_rustls()
            .no_proxy()
            .resolve(hostname, address);
        if trust {
            let cert = reqwest::Certificate::from_der(include_bytes!("fixtures/ca.der")).unwrap();
            builder = builder.tls_certs_only([cert]);
        }
        let client = build_client(builder, false).unwrap();
        let endpoint = format!("https://{hostname}:{}/v1/messages", address.port());
        let cloud = AnthropicProvider::with_endpoint(ClaudeLane::Opus, "fixture-key", endpoint)
            .unwrap()
            .with_test_bounded_client(client);
        let result =
            cloud.complete_bounded(&authorize(&cloud, CompletionLimits::default()), &control());
        if trust && hostname == "localhost" {
            assert_eq!(result.unwrap().text, "{}");
            let observed = server.join().unwrap().unwrap();
            assert!(observed.headers.starts_with("POST /v1/messages HTTP/1.1"));
            assert_eq!(
                serde_json::from_slice::<Value>(&observed.body).unwrap()["max_tokens"],
                2048
            );
        } else {
            assert_eq!(result.unwrap_err(), unknown(FailureCode::Transport));
            assert!(
                server.join().unwrap().is_none(),
                "invalid TLS must not carry an HTTP request"
            );
        }
    }
}

// AC-0570: the hosted output allowance is one exact value admitted only by the
// investigation constructor and stored-record decoding; the general ceiling
// and every other limit are unchanged.
#[test]
fn hosted_output_allowance_is_exact_and_separate_from_the_general_ceiling() {
    let mut values = CompletionLimits::default().values();
    values.max_output_tokens = HOSTED_OUTPUT_TOKENS;
    assert_eq!(
        CompletionLimits::new(values).unwrap_err().code,
        FailureCode::InvalidLimits
    );
    let hosted = CompletionLimits::for_investigation(values).unwrap();
    assert_eq!(hosted.values(), values);
    #[derive(serde::Deserialize)]
    struct Stored {
        #[serde(deserialize_with = "deserialize_investigation_limits")]
        limits: CompletionLimits,
    }
    let stored = |values: CompletionLimitValues| {
        serde_json::from_value::<Stored>(json!({ "limits": values })).map(|s| s.limits)
    };
    assert_eq!(stored(values).unwrap(), hosted);
    // Ordinary decoding keeps the general ceiling.
    assert!(
        serde_json::from_value::<CompletionLimits>(serde_json::to_value(values).unwrap()).is_err()
    );
    for tokens in [
        2049,
        8192,
        HOSTED_OUTPUT_TOKENS - 1,
        HOSTED_OUTPUT_TOKENS + 1,
    ] {
        values.max_output_tokens = tokens;
        assert!(
            CompletionLimits::for_investigation(values).is_err(),
            "{tokens}"
        );
        assert!(stored(values).is_err(), "{tokens}");
    }
    values.max_output_tokens = HOSTED_OUTPUT_TOKENS;
    values.output_text_bytes += 1;
    assert!(CompletionLimits::for_investigation(values).is_err());
    values.output_text_bytes -= 1;
    values.max_output_tokens = 2048;
    assert_eq!(
        CompletionLimits::for_investigation(values).unwrap(),
        CompletionLimits::new(values).unwrap()
    );
}

#[test]
fn bounded_span_limits_require_explicit_capacity_and_reject_excess() {
    // AC-0281: expanded source counts require a validated declaration; the
    // default firewall retains its legacy12 allowance. No request is dispatched.
    let local = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::local_only());
    let spans = |count: usize| {
        let mut input = action();
        input.payload.spans = (0..count)
            .map(|index| {
                let mut span = input.payload.spans[0].clone();
                span.id = format!("evidence-{index}");
                span
            })
            .collect();
        input
    };
    firewall
        .prepare_bounded(&local, &spans(12), &CompletionLimits::default())
        .unwrap();
    assert_eq!(
        firewall
            .prepare_bounded(&local, &spans(13), &CompletionLimits::default())
            .unwrap_err(),
        not_sent(FailureCode::InvalidInput)
    );
    let expanded = CompletionLimits::new(CompletionLimitValues {
        max_spans: 37,
        ..Default::default()
    })
    .unwrap();
    let prepared = firewall
        .prepare_bounded(&local, &spans(37), &expanded)
        .unwrap();
    assert_eq!(prepared.preview().egress.payload.spans.len(), 37);
    assert_eq!(prepared.limits().values().max_spans, 37);
    assert_eq!(
        firewall
            .prepare_bounded(&local, &spans(38), &expanded)
            .unwrap_err(),
        not_sent(FailureCode::InvalidInput)
    );
    let maximum = CompletionLimits::new(CompletionLimitValues {
        max_spans: 56,
        ..Default::default()
    })
    .unwrap();
    firewall
        .prepare_bounded(&local, &spans(56), &maximum)
        .unwrap();
    assert_eq!(
        firewall
            .prepare_bounded(&local, &spans(57), &maximum)
            .unwrap_err(),
        not_sent(FailureCode::InvalidInput)
    );
    for max_spans in [0, 57] {
        let values = CompletionLimitValues {
            max_spans,
            ..Default::default()
        };
        assert_eq!(
            CompletionLimits::new(values).unwrap_err(),
            not_sent(FailureCode::InvalidLimits)
        );
        assert!(
            serde_json::from_str::<CompletionLimits>(&serde_json::to_string(&values).unwrap())
                .is_err()
        );
    }
}

#[test]
fn bounded_span_limits_preserve_legacy_wire_and_consent_identity() {
    // AC-0281: omitted/default12 bytes and old hash recipe remain exact; a
    // different explicit span allowance changes the consent-bound identity.
    let literal = r#"{"input_bytes":131072,"request_bytes":524288,"response_bytes":262144,"output_text_bytes":32768,"max_output_tokens":2048,"request_timeout_ms":180000}"#;
    let legacy: CompletionLimits = serde_json::from_str(literal).unwrap();
    assert_eq!(legacy.values().max_spans, 12);
    assert_eq!(legacy, CompletionLimits::default());
    assert_eq!(serde_json::to_string(&legacy).unwrap(), literal);
    assert_eq!(serde_json::to_string(&legacy.values()).unwrap(), literal);
    let explicit12 = literal.replace('}', ",\"max_spans\":12}");
    assert_eq!(
        serde_json::from_str::<CompletionLimits>(&explicit12).unwrap(),
        legacy
    );
    let cloud = AnthropicProvider::with_endpoint(
        ClaudeLane::Opus,
        "fixture-key",
        "https://example.invalid/".into(),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let input = action();
    let prepared = firewall.prepare_bounded(&cloud, &input, &legacy).unwrap();
    #[derive(serde::Serialize)]
    struct LegacyLimits {
        input_bytes: u64,
        request_bytes: u64,
        response_bytes: u64,
        output_text_bytes: u64,
        max_output_tokens: u32,
        request_timeout_ms: u64,
    }
    let old_values = LegacyLimits {
        input_bytes: 131072,
        request_bytes: 524288,
        response_bytes: 262144,
        output_text_bytes: 32768,
        max_output_tokens: 2048,
        request_timeout_ms: 180000,
    };
    let old_hash_bytes = serde_json::to_vec(&(
        "cartograph-bounded-egress-v1",
        prepared.profile(),
        old_values,
        input.tier,
        &input.action_id,
        &prepared.preview().egress.payload,
    ))
    .unwrap();
    assert_eq!(
        prepared.preview().egress.payload_hash,
        blake3::hash(&old_hash_bytes).to_hex().to_string()
    );
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    let expanded = CompletionLimits::new(CompletionLimitValues {
        max_spans: 37,
        ..legacy.values()
    })
    .unwrap();
    let encoded = serde_json::to_string(&expanded).unwrap();
    assert!(encoded.ends_with(",\"max_spans\":37}"));
    assert_eq!(
        serde_json::from_str::<CompletionLimits>(&encoded).unwrap(),
        expanded
    );
    let changed = firewall.prepare_bounded(&cloud, &input, &expanded).unwrap();
    assert_ne!(
        prepared.preview().egress.payload_hash,
        changed.preview().egress.payload_hash
    );
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    firewall
        .authorize_bounded(&cloud, &prepared, Some(&grant))
        .unwrap();
}

// AC-0327: independently declared wire/output caps compose monotonically.
#[test]
fn transport_workspace_tracks_independent_limits() {
    let values = CompletionLimitValues {
        input_bytes: 1024,
        request_bytes: 2048,
        response_bytes: 4096,
        output_text_bytes: 512,
        ..Default::default()
    };
    let base = CompletionLimits::new(values)
        .unwrap()
        .transport_workspace_bytes()
        .unwrap();
    for field in 0..4 {
        let mut larger = values;
        match field {
            0 => larger.input_bytes *= 2,
            1 => larger.request_bytes *= 2,
            2 => larger.response_bytes *= 2,
            _ => larger.output_text_bytes *= 2,
        }
        let next = CompletionLimits::new(larger)
            .unwrap()
            .transport_workspace_bytes()
            .unwrap();
        assert!(next.request_peak_bytes >= base.request_peak_bytes);
        assert!(next.response_peak_bytes > base.response_peak_bytes);
        assert!(next.completion_bytes >= base.completion_bytes);
        if field == 0 || field == 1 {
            assert!(next.request_peak_bytes > base.request_peak_bytes);
        }
        if field == 3 {
            assert!(next.completion_bytes > base.completion_bytes);
        }
    }
    assert!(
        CompletionLimits::new(CompletionLimitValues {
            response_bytes: 0,
            ..values
        })
        .is_err()
    );
}

// AC-0327: the 32-block semantic limit cannot bound pre-validation decoding.
#[test]
fn transport_workspace_covers_rejected_response_owners() {
    use super::response_wire::{AnthropicBlock, AnthropicResponse, OllamaResponse};
    use std::mem::size_of;
    let blocks = vec![serde_json::json!({"type":"text","text":"é\n".repeat(128)}); 64];
    let wire = serde_json::to_vec(&serde_json::json!({
        "type":"message", "role":"assistant", "stop_reason":"end_turn", "content":blocks
    }))
    .unwrap();
    super::preflight_response(&wire).unwrap();
    let decoded: AnthropicResponse = serde_json::from_slice(&wire).unwrap();
    assert_eq!(decoded.content.len(), 64); // Production semantic check will refuse.
    let owned = size_of::<AnthropicResponse>()
        + decoded.content.len() * size_of::<AnthropicBlock>()
        + decoded
            .content
            .iter()
            .map(|b| b.kind.len() + b.text.as_ref().map_or(0, String::len))
            .sum::<usize>();
    let limits = CompletionLimits::new(CompletionLimitValues {
        response_bytes: wire.len() as u64,
        ..Default::default()
    })
    .unwrap();
    assert!(
        owned + wire.len()
            <= limits
                .transport_workspace_bytes()
                .unwrap()
                .response_peak_bytes
    );

    let wire = serde_json::to_vec(&serde_json::json!({
        "message":{"role":"assistant","content":"x","tool_calls":[{"arguments":vec!["é";1024]}]},
        "done":true,"done_reason":"stop"
    }))
    .unwrap();
    super::preflight_response(&wire).unwrap();
    let decoded: OllamaResponse = serde_json::from_slice(&wire).unwrap();
    assert!(!decoded.message.tool_calls.is_empty()); // Also rejected only after decode.
    let nested = &decoded.message.tool_calls[0]["arguments"];
    assert_eq!(nested.as_array().unwrap().len(), 1024);
    let owners = size_of::<OllamaResponse>() + 1024 * (size_of::<Value>() + "é".len());
    let limits = CompletionLimits::new(CompletionLimitValues {
        response_bytes: wire.len() as u64,
        ..Default::default()
    })
    .unwrap();
    assert!(
        owners + wire.len()
            <= limits
                .transport_workspace_bytes()
                .unwrap()
                .response_peak_bytes
    );
}

#[test]
fn firewall_workspace_covers_redacted_preparation_and_authorized_copy() {
    // AC-0327: firewall component only, not full task admission or RSS.
    // Constructing a provider never invokes it or opens a socket.
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = CompletionLimits::default();
    let mut original = action();
    original.payload.spans[0].text = "password=supersecret123".into();
    let prepared = firewall
        .prepare_bounded(&provider, &original, &limits)
        .unwrap();
    let authorized = firewall
        .authorize_bounded(&provider, &prepared, None)
        .unwrap();
    assert_ne!(
        original.payload.spans[0].text,
        authorized.payload().spans[0].text
    );
    let mut tiny = limits.values();
    tiny.input_bytes = 1;
    assert_eq!(
        firewall
            .prepare_bounded(&provider, &original, &CompletionLimits::new(tiny).unwrap())
            .unwrap_err()
            .code,
        FailureCode::InputTooLarge
    );
    let model = limits.firewall_workspace_bytes().unwrap();
    let actual = serde_json::to_vec(prepared.preview()).unwrap().len();
    assert!(model.prepared_bytes >= actual);
    assert!(model.authorized_bytes >= serde_json::to_vec(authorized.payload()).unwrap().len());
    assert!(model.prepare_peak_bytes >= model.prepared_bytes + 512 * 1024);
    assert!(model.authorize_peak_bytes > model.authorized_bytes);
    let mut values = limits.values();
    values.input_bytes /= 2;
    values.max_spans = 1;
    let smaller = CompletionLimits::new(values)
        .unwrap()
        .firewall_workspace_bytes()
        .unwrap();
    assert!(smaller.prepared_bytes < model.prepared_bytes);
    assert!(smaller.prepare_peak_bytes < model.prepare_peak_bytes);
    values.input_bytes = u64::MAX;
    assert!(CompletionLimits::new(values).is_err());
}

fn openai_body(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id":"chatcmpl-fixture", "object":"chat.completion", "model":"test-model",
        "choices":[{"index":0, "message":{"role":"assistant", "content":text,
            "refusal":null, "tool_calls":[], "function_call":null, "reasoning":"PRIVATE THOUGHT"},
            "finish_reason":"stop"}],
        "usage":{"prompt_tokens":19, "completion_tokens":5, "total_tokens":24},
    }))
    .unwrap()
}

fn openai(endpoint: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(endpoint, "test-model", Duration::from_secs(10)).unwrap()
}

// #938: a local OpenAI-compatible server gets the same finite generation limit,
// identity encoding and consented payload; its id, model and usage are kept.
#[test]
fn openai_compatible_bounded_sends_limits_and_reads_its_choice() {
    let limits = CompletionLimits::new(CompletionLimitValues {
        max_output_tokens: 37,
        ..Default::default()
    })
    .unwrap();
    let body = openai_body(r#"{"action":"query_context"}"#);
    let (endpoint, server) = http(framed(&body));
    let provider = openai(&endpoint);
    let profile = provider.bounded_profile().unwrap();
    assert_eq!(
        profile.endpoint_id,
        format!("{endpoint}v1/chat/completions")
    );
    assert_eq!(profile.locality, Locality::Local);
    let output = provider
        .complete_bounded(&authorize(&provider, limits), &control())
        .unwrap();
    let request = server.join().unwrap();
    assert!(request.headers.starts_with("POST /v1/chat/completions "));
    let headers = request.headers.to_ascii_lowercase();
    assert!(headers.contains("accept-encoding: identity"));
    assert!(!headers.contains("authorization"));
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["max_tokens"], 37);
    assert_eq!(wire["model"], "test-model");
    assert_eq!(wire["stream"], false);
    assert_eq!(wire["response_format"], json!({"type":"json_object"}));
    assert!(wire.get("tools").is_none());
    assert!(
        wire["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("if (blocked) return false;")
    );
    assert_eq!(output.text, r#"{"action":"query_context"}"#);
    assert_eq!(
        output.provider_request_id.as_deref(),
        Some("chatcmpl-fixture")
    );
    assert_eq!(output.response_model.as_deref(), Some("test-model"));
    let usage = output.usage.as_ref().unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(19), Some(5))
    );
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, body.len() as u64);
    let workspace = limits.transport_workspace_bytes().unwrap();
    assert!(request.body.len() <= workspace.request_peak_bytes);
    assert!(body.len() <= workspace.response_peak_bytes);
    assert!(serde_json::to_vec(&output).unwrap().len() <= workspace.completion_bytes);
    assert!(!format!("{output:?}").contains("PRIVATE THOUGHT"));
}

// #938: truncation, refusals, tool calls and unknown envelopes fail closed
// without echoing provider content.
#[test]
fn openai_compatible_rejects_truncated_refused_and_unknown_envelopes() {
    let base: Value = serde_json::from_slice(&openai_body("{}")).unwrap();
    let mut cases = Vec::new();
    let mut case = |edit: &dyn Fn(&mut Value), expected| {
        let mut body = base.clone();
        edit(&mut body);
        cases.push((body, expected));
    };
    case(
        &|b| b["choices"][0]["finish_reason"] = json!("length"),
        FailureCode::OutputLimitReached,
    );
    // Precedence: a refusal wins over the output limit.
    case(
        &|b| {
            b["choices"][0]["finish_reason"] = json!("length");
            b["choices"][0]["message"]["refusal"] = json!("PRIVATE SOURCE");
        },
        FailureCode::Refused,
    );
    case(
        &|b| b["choices"][0]["finish_reason"] = Value::Null,
        FailureCode::IncompleteResponse,
    );
    case(
        &|b| b["choices"][0]["finish_reason"] = json!("content_filter"),
        FailureCode::Refused,
    );
    case(
        &|b| b["choices"][0]["message"]["refusal"] = json!("PRIVATE SOURCE"),
        FailureCode::Refused,
    );
    case(
        &|b| {
            b["choices"][0]["message"]["tool_calls"] =
                json!([{"function":{"name":"shell","arguments":"PRIVATE SOURCE"}}])
        },
        FailureCode::InvalidResponse,
    );
    case(
        &|b| b["choices"][0]["message"]["function_call"] = json!({"name":"shell"}),
        FailureCode::InvalidResponse,
    );
    case(
        &|b| b["choices"][0]["message"]["role"] = json!("tool"),
        FailureCode::InvalidResponse,
    );
    case(
        &|b| {
            let choice = b["choices"][0].clone();
            b["choices"] = json!([choice.clone(), choice]);
        },
        FailureCode::InvalidResponse,
    );
    case(&|b| b["choices"] = json!([]), FailureCode::InvalidResponse);
    case(
        &|b| b["object"] = json!("chat.completion.chunk"),
        FailureCode::InvalidResponse,
    );
    case(
        &|b| b["choices"][0]["message"]["content"] = Value::Null,
        FailureCode::EmptyCompletion,
    );
    case(
        &|b| b["usage"]["completion_tokens"] = json!(2049),
        FailureCode::InvalidResponse,
    );
    for (body, expected) in cases {
        let (endpoint, server) = http(framed(&serde_json::to_vec(&body).unwrap()));
        let provider = openai(&endpoint);
        let error = provider
            .complete_bounded(
                &authorize(&provider, CompletionLimits::default()),
                &control(),
            )
            .unwrap_err();
        assert_eq!(error, rejected(expected), "{body}");
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
}

// #938: a declared local provider is plain-HTTP loopback only, so a LAN or
// HTTPS host is refused at construction, and a caller-added proxy is ignored.
#[test]
fn openai_compatible_is_loopback_only_and_ignores_proxies() {
    for endpoint in [
        "http://192.168.65.177:8000/",
        "http://spark.lan:8000/",
        "https://127.0.0.1:8000/",
    ] {
        assert!(matches!(
            OpenAiCompatibleProvider::new(endpoint, "m", Duration::from_secs(1)),
            Err(ProviderError::NonLoopback(_))
        ));
    }
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let (endpoint, server) = http(framed(&openai_body("{}")));
    let mut provider = openai(&endpoint);
    provider.bounded_client = build_client(
        Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{}", proxy.local_addr().unwrap())).unwrap()),
        true,
    )
    .unwrap();
    assert_eq!(
        provider
            .complete_bounded(
                &authorize(&provider, CompletionLimits::default()),
                &control()
            )
            .unwrap()
            .text,
        "{}"
    );
    server.join().unwrap();
    assert_no_request(&proxy);
}

// #938: the served model ids are listed with a bounded read; embeddings are
// unsupported rather than silently routed elsewhere.
#[test]
fn openai_compatible_lists_models_and_declines_embeddings() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        set_timeouts(&stream);
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
        }
        let body = br#"{"object":"list","data":[{"id":"nemotron-lightning","object":"model","max_model_len":262144}]}"#;
        stream.write_all(&framed(body)).unwrap();
        String::from_utf8(headers).unwrap()
    });
    let provider = openai(&endpoint);
    assert_eq!(provider.list_models().unwrap(), ["nemotron-lightning"]);
    assert!(server.join().unwrap().starts_with("GET /v1/models "));
    assert!(!provider.capabilities().embeddings);
    assert!(matches!(
        provider.embed(&["x".into()]),
        Err(ProviderError::Unsupported("embeddings"))
    ));
    assert_eq!(
        provider.completion_model_id(),
        "openai-compatible:test-model"
    );
}

// #943 review: the non-bounded local client never follows a redirect off
// loopback, and a bracketed IPv6 loopback base is Local.
#[test]
fn local_legacy_clients_never_follow_redirects_and_accept_ipv6_loopback() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    let location = format!(
        "http://{}/v1/chat/completions",
        target.local_addr().unwrap()
    );
    let redirect = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes();
    let request = crate::ProviderCompletionRequest {
        payload: action().payload,
    };
    let (endpoint, server) = http(redirect.clone());
    assert!(openai(&endpoint).complete(&request).is_err());
    server.join().unwrap();
    let (endpoint, server) = http(redirect);
    let ollama = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    assert!(ollama.complete(&request).is_err());
    server.join().unwrap();
    assert_no_request(&target);
    for endpoint in ["http://[::1]:8000/", "http://localhost:8000"] {
        assert!(OpenAiCompatibleProvider::new(endpoint, "m", Duration::from_secs(1)).is_ok());
        assert!(OllamaProvider::new(endpoint, "m", Duration::from_secs(1)).is_ok());
    }
}

// #943 review (Codex): the legacy completion shares the bounded envelope
// admission, so only one assistant choice that stopped normally completes.
#[test]
fn openai_compatible_legacy_completion_admits_only_a_stopped_assistant_choice() {
    let request = crate::ProviderCompletionRequest {
        payload: action().payload,
    };
    let base: Value = serde_json::from_slice(&openai_body(r#"{"action":"finish"}"#)).unwrap();
    let (endpoint, server) = http(framed(&serde_json::to_vec(&base).unwrap()));
    assert_eq!(
        openai(&endpoint).complete(&request).unwrap().text,
        r#"{"action":"finish"}"#
    );
    let wire: Value = serde_json::from_slice(&server.join().unwrap().body).unwrap();
    assert_eq!(wire["response_format"], json!({"type":"json_object"}));
    type Edit<'a> = (&'a dyn Fn(&mut Value), bool);
    let edits: [Edit; 6] = [
        (
            &|b| b["choices"][0]["finish_reason"] = json!("length"),
            false,
        ),
        (
            &|b| b["choices"][0]["finish_reason"] = json!("content_filter"),
            true,
        ),
        (
            &|b| b["choices"][0]["message"]["refusal"] = json!("PRIVATE SOURCE"),
            true,
        ),
        (
            &|b| b["choices"][0]["message"]["role"] = json!("tool"),
            false,
        ),
        (
            &|b| b["choices"][0]["message"]["tool_calls"] = json!([{"function":{"name":"shell"}}]),
            false,
        ),
        (&|b| b["choices"] = json!([]), false),
    ];
    for (edit, refused) in edits {
        let mut body = base.clone();
        edit(&mut body);
        let (endpoint, server) = http(framed(&serde_json::to_vec(&body).unwrap()));
        let error = openai(&endpoint).complete(&request).unwrap_err();
        assert_eq!(
            matches!(error, ProviderError::Refused { .. }),
            refused,
            "{body}"
        );
        assert!(refused || matches!(error, ProviderError::InvalidResponse(_)));
        assert!(!format!("{error:?} {error}").contains("PRIVATE SOURCE"));
        server.join().unwrap();
    }
}

#[test]
fn bounded_response_schema_is_bound_bounded_and_explicit() {
    // AC-0185: no provider invocation; exercise production preparation/wire gates.
    let provider = OllamaProvider::new(
        "http://127.0.0.1:11434",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = CompletionLimits::default();
    let mut original = action();
    let legacy = serde_json::to_value(&original.payload).unwrap();
    assert_eq!(
        legacy,
        json!({"system":original.payload.system,"prompt":original.payload.prompt,"spans":original.payload.spans})
    );
    let old = firewall
        .prepare_bounded(&provider, &original, &limits)
        .unwrap();
    let schema = RESPONSE_SCHEMA;
    original.payload.response_schema = Some(schema.into());
    let prepared = firewall
        .prepare_bounded(&provider, &original, &limits)
        .unwrap();
    assert!(!old.matches_action(&original));
    assert!(!ConsentGrant::from_preview(&old.preview().egress).matches(&prepared.preview().egress));
    assert_eq!(
        prepared.preview().egress.payload.response_schema.as_deref(),
        Some(schema)
    );
    let authorized = firewall
        .authorize_bounded(&provider, &prepared, None)
        .unwrap();
    let wire = request_wire::ollama(
        "test-model",
        "system",
        "user",
        2048,
        response_schema(authorized.payload()).unwrap(),
        false,
        None,
    );
    let bytes = encode(
        &wire,
        limits.values().request_bytes,
        FailureCode::RequestTooLarge,
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["format"],
        serde_json::from_str::<Value>(schema).unwrap()
    );
    assert_eq!(
        encode(&wire, bytes.len() as u64 - 1, FailureCode::RequestTooLarge).unwrap_err(),
        not_sent(FailureCode::RequestTooLarge)
    );
    original.payload.response_schema = Some(format!(" {schema}"));
    assert!(!prepared.matches_action(&original));
    let whitespace = firewall
        .prepare_bounded(&provider, &original, &limits)
        .unwrap();
    assert_ne!(
        prepared.preview().egress.payload_hash,
        whitespace.preview().egress.payload_hash
    );
    for rejected in [
        "[]",
        "{} trailing",
        r#"{"description":"password=secret123456"}"#,
        r#"{"description":"\u0070assword=secret123456"}"#,
    ] {
        original.payload.response_schema = Some(rejected.into());
        assert_eq!(
            firewall
                .prepare_bounded(&provider, &original, &limits)
                .unwrap_err(),
            not_sent(FailureCode::InvalidInput)
        );
    }
    original.payload.response_schema = Some("x".repeat(limits.values().input_bytes as usize));
    assert_eq!(
        firewall
            .prepare_bounded(&provider, &original, &limits)
            .unwrap_err(),
        not_sent(FailureCode::InputTooLarge)
    );
    original.payload.response_schema = Some(schema.into());
    let other = OpenAiCompatibleProvider::new(
        "http://127.0.0.1:8000",
        "test-model",
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(firewall.prepare_bounded(&other, &original, &limits).is_ok());
    assert!(matches!(
        firewall.complete(&provider, &original, None),
        Err(ProviderError::Unsupported("legacy response schema"))
    ));
}

// AC-0560 (#1624): with a debug sink installed, each bounded call records a
// SENT entry carrying the exact wire request before the response arrives,
// and a RECEIVED entry with the exact response body or the failure code.
#[test]
fn bounded_calls_record_sent_then_received_in_the_debug_log() {
    crate::debug_log::test_support::capture();
    let limits = CompletionLimits::new(CompletionLimitValues::default()).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let run = |marker: &str, response: Vec<u8>| {
        let (endpoint, server) = http(response);
        let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
        let mut selected = action();
        selected.payload.prompt = format!("Investigate {marker}.");
        let prepared = firewall
            .prepare_bounded(&local, &selected, &limits)
            .unwrap();
        let authorized = firewall.authorize_bounded(&local, &prepared, None).unwrap();
        let result = local.complete_bounded(&authorized, &control());
        let request = server.join().unwrap();
        (result, request, endpoint)
    };

    let body = ollama_body(r#"{"type":"replay_query"}"#);
    let (result, request, endpoint) = run("debug-1624-ok", framed(&body));
    assert!(result.is_ok());
    let calls = crate::debug_log::test_support::calls_with("debug-1624-ok");
    assert_eq!(calls.len(), 1);
    let (sent, received) = &calls[0];
    assert_eq!(sent.path, "bounded");
    assert_eq!(sent.target, format!("{endpoint}api/chat"));
    assert_eq!(
        sent.request.as_deref().unwrap().as_bytes(),
        request.body.as_slice()
    );
    let received = received.as_ref().expect("received entry");
    assert_eq!(received.phase, crate::debug_log::Phase::Received);
    assert_eq!(
        received.response.as_deref().unwrap().as_bytes(),
        body.as_slice()
    );
    assert_eq!(received.error, None);
    assert!(received.call == sent.call && received.request.is_none());

    let failure =
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
    let (result, _, _) = run("debug-1624-fail", failure);
    assert!(result.is_err());
    let calls = crate::debug_log::test_support::calls_with("debug-1624-fail");
    let received = calls[0].1.as_ref().expect("received entry");
    assert_eq!(received.error.as_deref(), Some("HttpStatus"));
    assert_eq!(received.response, None);

    // #1624 review: a 200 whose envelope admission refuses is logged with
    // that refusal and the exact body, not as OK.
    let malformed = br#"{"model":"test-model","done":false}"#;
    let (result, _, _) = run("debug-1624-envelope", framed(malformed));
    let code = format!("{:?}", result.unwrap_err().code);
    let calls = crate::debug_log::test_support::calls_with("debug-1624-envelope");
    let received = calls[0].1.as_ref().expect("received entry");
    assert_eq!(received.error.as_deref(), Some(code.as_str()));
    assert_eq!(
        received.response.as_deref().unwrap().as_bytes(),
        malformed.as_slice()
    );
}

// AC-0560 (#1624 review): embedding calls are logged too, with the input
// texts as SENT and a vector summary (not the values) as RECEIVED.
#[test]
fn embedding_calls_record_texts_and_a_vector_summary_in_the_debug_log() {
    crate::debug_log::test_support::capture();
    let body = br#"{"model":"test-model","embeddings":[[0.5,0.25,0.125],[1.0,0.0,0.0]]}"#;
    let (endpoint, server) = http(framed(body));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let texts = vec!["debug-1624-embed alpha".to_string(), "beta".to_string()];
    let vectors = local.embed(&texts).unwrap();
    server.join().unwrap();
    assert_eq!(vectors.len(), 2);
    let calls = crate::debug_log::test_support::calls_with("debug-1624-embed");
    let (sent, received) = &calls[0];
    assert_eq!(sent.path, "embed");
    assert_eq!(sent.target, format!("{endpoint}api/embed"));
    let request = sent.request.as_deref().unwrap();
    assert!(request.contains("texts=2") && request.contains("--- text ---\nbeta"));
    let received = received.as_ref().expect("received entry");
    assert_eq!(
        received.response.as_deref(),
        Some("2 vectors of 3 dimensions (values not shown)")
    );
    assert_eq!(received.error, None);
}

// AC-0560 (#1624): the control check and deadline run after the debug entry
// is queued, so cancellation arriving at that instant sends nothing, and a
// slow log writer never delays or blocks a provider call.
#[test]
fn debug_logging_precedes_the_final_control_check_and_never_blocks_the_call() {
    use crate::debug_log::test_support::{BEGUN_ON_THREAD, HOLD_SLOW_SINK};
    crate::debug_log::test_support::capture();
    let limits = CompletionLimits::new(CompletionLimitValues::default()).unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let authorized = |local: &OllamaProvider, marker: &str| {
        let mut selected = action();
        selected.payload.prompt = format!("Investigate {marker}.");
        let prepared = firewall.prepare_bounded(local, &selected, &limits).unwrap();
        firewall.authorize_bounded(local, &prepared, None).unwrap()
    };

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = OllamaProvider::new(
        &format!("http://{}/", listener.local_addr().unwrap()),
        "test-model",
        Duration::from_secs(10),
    )
    .unwrap();
    let request = authorized(&local, "debug-1624-cancel-after-log");
    let before = BEGUN_ON_THREAD.with(std::cell::Cell::get);
    let check = || {
        if BEGUN_ON_THREAD.with(std::cell::Cell::get) > before {
            CallDirective::Cancelled
        } else {
            CallDirective::Continue
        }
    };
    let control = CompletionControl {
        deadline: Instant::now() + Duration::from_secs(5),
        check: &check,
        dispatching: None,
    };
    assert_eq!(
        local.complete_bounded(&request, &control).unwrap_err(),
        not_sent(FailureCode::Cancelled)
    );
    assert_no_request(&listener);
    let calls = crate::debug_log::test_support::calls_with("debug-1624-cancel-after-log");
    assert_eq!(calls.len(), 1);
    let received = calls[0].1.as_ref().expect("received entry");
    assert_eq!(received.error.as_deref(), Some("Cancelled"));

    let (endpoint, server) = http(framed(&ollama_body(r#"{"action":"finish"}"#)));
    let local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    let request = authorized(&local, "debug-1624-slow-sink");
    HOLD_SLOW_SINK.with(|hold| hold.store(true, Ordering::Release));
    let started = Instant::now();
    let result = local.complete_bounded(
        &request,
        &CompletionControl {
            deadline: Instant::now() + Duration::from_secs(2),
            check: &proceed,
            dispatching: None,
        },
    );
    let elapsed = started.elapsed();
    HOLD_SLOW_SINK.with(|hold| hold.store(false, Ordering::Release));
    server.join().unwrap();
    assert_eq!(result.unwrap().text, r#"{"action":"finish"}"#);
    assert!(elapsed < Duration::from_secs(2));
    let calls = crate::debug_log::test_support::calls_with("debug-1624-slow-sink");
    assert!(calls[0].1.as_ref().is_some_and(|r| r.error.is_none()));
}

#[test]
fn bounded_timeout_ceiling_is_separate_from_unchanged_default() {
    // AC-0188: saved completion limits round-trip with unchanged default bytes.
    assert_eq!(
        CompletionLimits::default().values().request_timeout_ms,
        180_000
    );
    for timeout in [1, 180_000, 180_001, 600_000] {
        let values = CompletionLimitValues {
            request_timeout_ms: timeout,
            ..Default::default()
        };
        let admitted = CompletionLimits::new(values).unwrap();
        let wire = serde_json::to_string(&admitted).unwrap();
        assert_eq!(
            serde_json::from_str::<CompletionLimits>(&wire).unwrap(),
            admitted
        );
    }
    for timeout in [0, 600_001, u64::MAX] {
        let values = CompletionLimitValues {
            request_timeout_ms: timeout,
            ..Default::default()
        };
        assert!(CompletionLimits::new(values).is_err());
        assert!(
            serde_json::from_str::<CompletionLimits>(&serde_json::to_string(&values).unwrap())
                .is_err()
        );
    }
    let mut values = CompletionLimitValues {
        request_timeout_ms: 600_000,
        ..Default::default()
    };
    values.input_bytes += 1;
    assert!(CompletionLimits::new(values).is_err());
}

#[test]
fn configured_timeout_changes_consent_identity_without_provider_execution() {
    // AC-0188: widening time cannot reuse consent for the original limits.
    let cloud = AnthropicProvider::with_endpoint(
        ClaudeLane::Opus,
        "fixture-key",
        "https://example.invalid/".into(),
    )
    .unwrap();
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let input = action();
    let baseline = firewall
        .prepare_bounded(&cloud, &input, &CompletionLimits::default())
        .unwrap();
    let grant = ConsentGrant::from_preview(&baseline.preview().egress);
    let limits = CompletionLimits::new(CompletionLimitValues {
        request_timeout_ms: MAX_REQUEST_TIMEOUT_MS,
        ..Default::default()
    })
    .unwrap();
    let changed = firewall.prepare_bounded(&cloud, &input, &limits).unwrap();
    assert_ne!(
        baseline.preview().egress.payload_hash,
        changed.preview().egress.payload_hash
    );
    assert_eq!(
        firewall
            .authorize_bounded(&cloud, &changed, Some(&grant))
            .err()
            .unwrap(),
        not_sent(FailureCode::ConsentMismatch)
    );
    let fresh = ConsentGrant::from_preview(&changed.preview().egress);
    firewall
        .authorize_bounded(&cloud, &changed, Some(&fresh))
        .unwrap();
}

// AC-0342 / #1654: actual synthetic HTTP bytes retain schema and bounded metrics.
#[test]
fn openai_compatible_strict_schema_and_thinking_are_bounded() {
    let duplicate_finish =
        include_str!("../../tests/fixtures/duplicate-citation-finish.json").trim();
    for thinking in [None, Some(false), Some(true)] {
        let mut body: Value = serde_json::from_slice(&openai_body(duplicate_finish)).unwrap();
        body["choices"][0]["message"]["reasoning_content"] = json!("PRIVATE REASONING");
        body["usage"]["prompt_tokens_details"] = json!({"cached_tokens": 11});
        let (endpoint, server) = http(framed(&serde_json::to_vec(&body).unwrap()));
        let base = openai(&endpoint);
        let base_profile = base.bounded_profile().unwrap();
        let provider = match thinking {
            Some(true) => base.with_thinking_enabled(),
            Some(false) => base.with_thinking_disabled(),
            None => base,
        };
        assert_eq!(
            provider.bounded_profile().unwrap() == base_profile,
            thinking.is_none()
        );
        assert!(provider.supports_bounded_response_schema());
        let firewall = EgressFirewall::new(EgressPolicy::default());
        let mut selected = action();
        selected.payload.response_schema =
            Some(r#"{"type":"array","uniqueItems":true,"items":{"type":"string"}}"#.into());
        let prepared = firewall
            .prepare_bounded(&provider, &selected, &CompletionLimits::default())
            .unwrap();
        let authorized = firewall
            .authorize_bounded(&provider, &prepared, None)
            .unwrap();
        let output = provider.complete_bounded(&authorized, &control()).unwrap();
        let request = server.join().unwrap();
        let wire: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(
            wire["response_format"],
            json!({"type":"json_schema", "json_schema":{
            "name":"cartograph_bounded_response", "strict":true,
            "schema":json!({"type":"array","items":{"type":"string"}})}})
        );
        if let Some(enabled) = thinking {
            assert_eq!(
                wire["chat_template_kwargs"],
                json!({"enable_thinking":enabled})
            );
        } else {
            assert!(wire.get("chat_template_kwargs").is_none());
        }
        assert_eq!(output.text, duplicate_finish);
        assert_eq!(output.usage.unwrap().cache_read_input_tokens, Some(11));
        assert_eq!(output.request_bytes, request.body.len() as u64);
    }
}

#[test]
fn ollama_reasoning_auditor_only_returns_content() {
    let mut body: Value = serde_json::from_slice(&ollama_body("answer")).unwrap();
    body["message"]["thinking"] = json!("private reasoning");
    body["eval_count"] = json!(6807);
    let bytes = serde_json::to_vec(&body).unwrap();
    let (endpoint, server) = http(framed(&bytes));
    let provider = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(2))
        .unwrap()
        .with_thinking_enabled();
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let limits = CompletionLimits::default().for_semantic_audit();
    let prepared = firewall
        .prepare_bounded(&provider, &action(), &limits)
        .unwrap();
    let authorized = firewall
        .authorize_bounded(&provider, &prepared, None)
        .unwrap();
    let output = provider.complete_bounded(&authorized, &control()).unwrap();
    let wire: Value = serde_json::from_slice(&server.join().unwrap().body).unwrap();
    assert_eq!(wire["think"], true);
    assert_eq!(wire["options"]["num_predict"], AUDIT_OUTPUT_TOKENS);
    assert_eq!(output.text, "answer");
    assert_eq!(output.usage.unwrap().output_tokens, Some(6807));
    assert_eq!(output.response_bytes, bytes.len() as u64);
}

#[test]
fn openai_compatible_oversize_schema_never_connects() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let provider =
        openai(&format!("http://{}/", listener.local_addr().unwrap())).with_thinking_disabled();
    let limits = CompletionLimits::new(CompletionLimitValues {
        request_bytes: 512,
        ..Default::default()
    })
    .unwrap();
    let mut selected = action();
    selected.payload.response_schema =
        Some(json!({"type":"object","description":"x".repeat(1024)}).to_string());
    let firewall = EgressFirewall::new(EgressPolicy::default());
    let prepared = firewall
        .prepare_bounded(&provider, &selected, &limits)
        .unwrap();
    let authorized = firewall
        .authorize_bounded(&provider, &prepared, None)
        .unwrap();
    assert_eq!(
        provider
            .complete_bounded(&authorized, &control())
            .unwrap_err(),
        not_sent(FailureCode::RequestTooLarge)
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn semantic_audit_limits_preserve_historical_reservations() {
    let current = CompletionLimits::default().for_semantic_audit();
    assert_eq!(current.values().max_output_tokens, 16384);
    for tokens in [2048, 8192, 16384] {
        let values = CompletionLimitValues {
            max_output_tokens: tokens,
            ..CompletionLimitValues::default()
        };
        assert_eq!(
            CompletionLimits::from_audit_values(values)
                .unwrap()
                .values()
                .max_output_tokens,
            tokens
        );
    }
}

fn openai_cloud(endpoint: &str, key: &str) -> crate::openai::OpenAiProvider {
    crate::openai::OpenAiProvider::with_endpoint(
        "gpt-fixture",
        key,
        format!("{endpoint}v1/chat/completions"),
    )
    .unwrap()
}

fn authorize_schema(provider: &dyn LlmProvider, schema: &str) -> AuthorizedBoundedCompletion {
    let firewall = EgressFirewall::new(EgressPolicy::allow_cloud_for([AnalysisTier::Agentic]));
    let mut selected = action();
    selected.payload.response_schema = Some(schema.into());
    let prepared = firewall
        .prepare_bounded(provider, &selected, &CompletionLimits::default())
        .unwrap();
    let grant = ConsentGrant::from_preview(&prepared.preview().egress);
    firewall
        .authorize_bounded(provider, &prepared, Some(&grant))
        .unwrap()
}

// AC-0567 / #1709: hosted OpenAI gets bearer auth, store:false, a reasoning-safe
// output bound and the wrapped strict schema; the host receives the exact inner
// bytes for full-schema validation.
#[test]
fn openai_cloud_bounded_wraps_schema_and_returns_inner_bytes() {
    const STRICT: &str = r#"{"anyOf":[{"type":"object","properties":{"type":{"enum":["query_context","read_evidence"]}},"required":["type"],"additionalProperties":false}]}"#;
    let inner = r#"{"type" : "query_context"}"#;
    let body = openai_body(&format!(r#"{{"response":{inner}}}"#));
    let (endpoint, server) = http(framed(&body));
    let provider = openai_cloud(&endpoint, "fixture-key");
    assert_eq!(provider.locality(), Locality::Cloud);
    assert_eq!(provider.id(), "openai:gpt-fixture");
    let output = provider
        .complete_bounded(&authorize_schema(&provider, STRICT), &control())
        .unwrap();
    let request = server.join().unwrap();
    assert!(request.headers.starts_with("POST /v1/chat/completions "));
    assert!(
        request
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-key")
    );
    let wire: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(wire["store"], false);
    assert_eq!(wire["stream"], false);
    assert_eq!(wire["model"], "gpt-fixture");
    assert!(wire.get("max_tokens").is_none());
    assert!(wire["max_completion_tokens"].as_u64().unwrap() > 0);
    assert!(wire.get("chat_template_kwargs").is_none());
    assert_eq!(wire["response_format"]["type"], "json_schema");
    assert_eq!(wire["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        wire["response_format"]["json_schema"]["schema"]["required"],
        json!(["response"])
    );
    assert_eq!(output.text, inner);
    assert_eq!(output.request_bytes, request.body.len() as u64);
    assert_eq!(output.response_bytes, body.len() as u64);
}

// AC-0567 / #1709: a wrong envelope, a refusal, an unusable key or an
// unrepresentable schema never becomes a completion.
#[test]
fn openai_cloud_bounded_fails_closed() {
    const STRICT: &str = r#"{"anyOf":[{"type":"object","properties":{"type":{"const":"x"}},"required":["type"],"additionalProperties":false}]}"#;
    let (endpoint, server) = http(framed(&openai_body(r#"{"type":"x"}"#)));
    let provider = openai_cloud(&endpoint, "fixture-key");
    let error = provider
        .complete_bounded(&authorize_schema(&provider, STRICT), &control())
        .unwrap_err();
    assert_eq!(error.code, FailureCode::InvalidResponse);
    server.join().unwrap();

    let mut refused: Value = serde_json::from_slice(&openai_body("")).unwrap();
    refused["choices"][0]["message"]["refusal"] = json!("no");
    let (endpoint, server) = http(framed(&serde_json::to_vec(&refused).unwrap()));
    let provider = openai_cloud(&endpoint, "fixture-key");
    let error = provider
        .complete_bounded(&authorize_schema(&provider, STRICT), &control())
        .unwrap_err();
    assert_eq!(error.code, FailureCode::Refused);
    server.join().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    for key in ["", "bad\nkey"] {
        let provider = openai_cloud(&endpoint, key);
        let error = provider
            .complete_bounded(&authorize_schema(&provider, STRICT), &control())
            .unwrap_err();
        assert_eq!(error.code, FailureCode::InvalidProfile, "{key:?}");
    }
    // `RESPONSE_SCHEMA` holds an open `{"type":"object"}`, which strict mode
    // cannot represent.
    let provider = openai_cloud(&endpoint, "fixture-key");
    let error = provider
        .complete_bounded(&authorize_schema(&provider, RESPONSE_SCHEMA), &control())
        .unwrap_err();
    assert_eq!(error.code, FailureCode::Unsupported);
    assert_no_request(&listener);
}

// AC-0820 (#1653): a bounded call is governed by its own declared
// `request_timeout_ms`, never by the reusable client's fallback timeout, and
// that fallback equals the configurable ceiling so a permitted call is never
// cut short.
#[test]
fn bounded_call_timeout_follows_declared_limit_not_client_fallback() {
    assert_eq!(
        CLIENT_FALLBACK_TIMEOUT,
        Duration::from_millis(MAX_REQUEST_TIMEOUT_MS)
    );
    let delayed = |delay: Duration| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            set_timeouts(&stream);
            read_request(&mut stream).unwrap();
            thread::sleep(delay);
            let _ = stream.write_all(&framed(&ollama_body("{}")));
        });
        (endpoint, server)
    };
    let limits = |request_timeout_ms| {
        CompletionLimits::new(CompletionLimitValues {
            request_timeout_ms,
            ..CompletionLimitValues::default()
        })
        .unwrap()
    };

    // A declared limit longer than the client fallback is honoured.
    let (endpoint, server) = delayed(Duration::from_millis(700));
    let mut local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    local.bounded_client =
        build_client_with_fallback(Client::builder(), true, Duration::from_millis(200)).unwrap();
    assert_eq!(
        local
            .complete_bounded(&authorize(&local, limits(5_000)), &control())
            .unwrap()
            .text,
        "{}"
    );
    server.join().unwrap();

    // A declared limit shorter than the client fallback still ends the call.
    let (endpoint, server) = delayed(Duration::from_millis(1_500));
    let mut local = OllamaProvider::new(&endpoint, "test-model", Duration::from_secs(10)).unwrap();
    local.bounded_client =
        build_client_with_fallback(Client::builder(), true, Duration::from_secs(30)).unwrap();
    let before = Instant::now();
    let error = local
        .complete_bounded(&authorize(&local, limits(300)), &control())
        .unwrap_err();
    assert_eq!(error, unknown(FailureCode::DeadlineExceeded));
    assert!(before.elapsed() < Duration::from_millis(1_400));
    server.join().unwrap();
}
