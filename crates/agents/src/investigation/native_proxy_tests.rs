// AC-1422 (ADR-0085): the loopback model proxy forwards one authorized request
// per step and refuses everything else before it reaches the provider.
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const LIMITS: CallLimits = CallLimits {
    max_request_bytes: 1 << 20,
    max_response_bytes: 1 << 20,
    max_output_tokens: 4096,
};

const AUTHORIZED: &str = "{\"changed\":{\"turn_progress\":{\"turn\":2}}}";

/// A stand-in provider that counts requests and answers one NDJSON line.
fn upstream() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let _ = read_request(&mut stream, 1 << 20);
            counter.fetch_add(1, Ordering::SeqCst);
            let body = "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true}\n";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, hits)
}

fn proxy(upstream: &str) -> ModelProxy {
    ModelProxy::bind(ProxyConfig {
        run_id: "run-7".into(),
        token: "run-7-token".into(),
        upstream: upstream.into(),
        backend: ProxyBackend::Ollama,
        model: "qwen3:30b-a3b".into(),
        context_window: Some(32768),
        max_request_bytes: 1 << 20,
        max_response_bytes: 1 << 20,
        credential: None,
    })
    .unwrap()
}

fn request(model: &str, num_ctx: u64, last: &str) -> Value {
    json!({"model": model, "stream": true, "options": {"num_ctx": num_ctx, "num_predict": 2048},
        "messages": [{"role": "system", "content": "s"}, {"role": "tool", "tool_name": "query_context", "content": last}]})
}

/// Sends one request as the runtime would and returns the response status.
fn send(base: String, path: &str, token: &str, body: Value) -> std::thread::JoinHandle<u16> {
    let path = path.to_owned();
    let token = token.to_owned();
    std::thread::spawn(move || {
        let url = format!("{base}{path}");
        let response = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .unwrap();
        response.status().as_u16()
    })
}

fn authorized() -> Vec<String> {
    vec![AUTHORIZED.to_owned()]
}

fn never() -> bool {
    false
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn an_authorized_request_is_recorded_then_forwarded() {
    let (url, hits) = upstream();
    let proxy = proxy(&url);
    let body = request("qwen3:30b-a3b", 32768, AUTHORIZED);
    let size = serde_json::to_vec(&body).unwrap().len() as u64;
    let client = send(proxy.base_url().unwrap(), "/api/chat", "run-7-token", body);
    let recorded = std::sync::Mutex::new(None);
    let exchange = proxy
        .serve_one(&authorized(), &LIMITS, soon(), &never, &|bytes| {
            *recorded.lock().unwrap() = Some(bytes);
            true
        })
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    assert_eq!(exchange.request_bytes, size);
    assert_eq!(
        *recorded.lock().unwrap(),
        Some(size),
        "the egress is recorded before forwarding"
    );
    assert_eq!(exchange.status, 200);
    assert!(exchange.response_bytes > 0);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn refused_requests_never_reach_the_provider() {
    let (url, hits) = upstream();
    let cases: Vec<(&str, &str, Value, ProxyRefusal, u16)> = vec![
        (
            "/api/chat",
            "other-token",
            request("qwen3:30b-a3b", 32768, AUTHORIZED),
            ProxyRefusal::Token,
            401,
        ),
        (
            "/v1/chat/completions",
            "run-7-token",
            request("qwen3:30b-a3b", 32768, AUTHORIZED),
            ProxyRefusal::Path,
            404,
        ),
        (
            "/api/chat",
            "run-7-token",
            request("llama3:70b", 32768, AUTHORIZED),
            ProxyRefusal::Model,
            403,
        ),
        (
            "/api/chat",
            "run-7-token",
            request("qwen3:30b-a3b", 2048, AUTHORIZED),
            ProxyRefusal::ContextWindow,
            403,
        ),
        (
            "/api/chat",
            "run-7-token",
            request("qwen3:30b-a3b", 32768, "text the host never sent"),
            ProxyRefusal::Unauthorized,
            403,
        ),
    ];
    for (path, token, body, refusal, status) in cases {
        let proxy = proxy(&url);
        let client = send(proxy.base_url().unwrap(), path, token, body);
        let dispatched = std::sync::atomic::AtomicBool::new(false);
        let outcome = proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| {
            dispatched.store(true, Ordering::SeqCst);
            true
        });
        assert_eq!(outcome, Err(refusal));
        assert_eq!(client.join().unwrap(), status);
        assert!(
            !dispatched.load(Ordering::SeqCst),
            "{refusal:?} is refused before any egress record"
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn an_unrecorded_egress_is_not_sent() {
    let (url, hits) = upstream();
    let proxy = proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| false),
        Err(ProxyRefusal::NotRecorded)
    );
    assert_eq!(client.join().unwrap(), 403);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn no_request_before_the_deadline_is_a_deadline_refusal() {
    let (url, _) = upstream();
    let proxy = proxy(&url);
    let deadline = Instant::now() + Duration::from_millis(30);
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, deadline, &never, &|_| true),
        Err(ProxyRefusal::Deadline)
    );
}

#[test]
fn the_vllm_backend_serves_the_openai_path_without_num_ctx() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (path, _, _) = read_request(&mut stream, 1 << 20).unwrap();
        let body = "data: [DONE]\n\n";
        write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}", body.len()).unwrap();
        path
    });
    let proxy = ModelProxy::bind(ProxyConfig {
        run_id: "run-8".into(),
        token: "t".into(),
        upstream: url,
        backend: ProxyBackend::OpenAiCompatible,
        model: "glm-5.3-flash".into(),
        context_window: Some(65536),
        max_request_bytes: 1 << 20,
        max_response_bytes: 1 << 20,
        credential: None,
    })
    .unwrap();
    let body = json!({"model": "glm-5.3-flash", "max_tokens": 2048, "messages": [{"role": "user", "content": AUTHORIZED}]});
    let client = send(proxy.base_url().unwrap(), "/v1/chat/completions", "t", body);
    proxy
        .serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true)
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    assert_eq!(seen.join().unwrap(), "/v1/chat/completions");
}

#[test]
fn every_message_after_the_models_answer_must_be_one_the_host_sent() {
    let (url, hits) = upstream();
    let extra = "Only one tool call per turn: this call was not run.".to_owned();
    let messages = json!([
        {"role": "system", "content": "s"},
        {"role": "user", "content": [{"type": "text", "text": "start"}]},
        {"role": "assistant", "content": null, "tool_calls": []},
        {"role": "tool", "content": AUTHORIZED},
        {"role": "tool", "content": extra},
    ]);
    let body = |messages: &Value| json!({"model": "qwen3:30b-a3b", "options": {"num_ctx": 32768, "num_predict": 2048}, "messages": messages});
    let both = vec![AUTHORIZED.to_owned(), extra.clone()];
    let proxy = proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        body(&messages),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true),
        Err(ProxyRefusal::Unauthorized),
        "a tool message the host did not authorize is refused"
    );
    assert_eq!(client.join().unwrap(), 403);
    let proxy = self::proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        body(&messages),
    );
    proxy
        .serve_one(&both, &LIMITS, soon(), &never, &|_| true)
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    let start = json!([{"role": "system", "content": "s"}, {"role": "user", "content": [{"type": "text", "text": "start"}]}]);
    let proxy = self::proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        body(&start),
    );
    proxy
        .serve_one(&["start".to_owned()], &LIMITS, soon(), &never, &|_| true)
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn an_interrupt_ends_the_wait_before_any_request() {
    let (url, _) = upstream();
    let proxy = proxy(&url);
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &|| true, &|_| true),
        Err(ProxyRefusal::Interrupted)
    );
}

#[test]
fn credentials_windows_and_request_lines_are_checked_exactly() {
    let (url, hits) = upstream();
    let proxy = proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "RUN-7-TOKEN",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true),
        Err(ProxyRefusal::Token),
        "the token is case-sensitive"
    );
    assert_eq!(client.join().unwrap(), 401);
    let proxy = ModelProxy::bind(ProxyConfig {
        context_window: None,
        ..self::proxy(&url).config.clone()
    })
    .unwrap();
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true),
        Err(ProxyRefusal::ContextWindow),
        "no window may be sent when none is frozen"
    );
    assert_eq!(client.join().unwrap(), 403);
    let proxy = self::proxy(&url);
    let address = proxy.listener.local_addr().unwrap();
    let flood = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        let _ = stream.write_all(format!("POST /{}", "a".repeat(64 * 1024)).as_bytes());
    });
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true),
        Err(ProxyRefusal::Request),
        "an endless request line is refused"
    );
    flood.join().unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn the_steps_own_limits_bind_before_any_egress() {
    let (url, hits) = upstream();
    let tight = CallLimits {
        max_output_tokens: 1024,
        ..LIMITS
    };
    let proxy = proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &tight, soon(), &never, &|_| true),
        Err(ProxyRefusal::OutputTokens),
        "a request may not ask for more output than the step allows"
    );
    assert_eq!(client.join().unwrap(), 403);
    let mut unbounded = request("qwen3:30b-a3b", 32768, AUTHORIZED);
    unbounded["options"]
        .as_object_mut()
        .unwrap()
        .remove("num_predict");
    let proxy = self::proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        unbounded,
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true),
        Err(ProxyRefusal::OutputTokens),
        "a request must ask for a limit"
    );
    assert_eq!(client.join().unwrap(), 403);
    let small = CallLimits {
        max_request_bytes: 64,
        ..LIMITS
    };
    let proxy = self::proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &small, soon(), &never, &|_| true),
        Err(ProxyRefusal::Request),
        "the step's request-byte limit applies"
    );
    let _ = client.join();
    let proxy = self::proxy(&url);
    let tiny = CallLimits {
        max_response_bytes: 8,
        ..LIMITS
    };
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        request("qwen3:30b-a3b", 32768, AUTHORIZED),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), &tiny, soon(), &never, &|_| true),
        Err(ProxyRefusal::Upstream),
        "the step's response-byte limit applies"
    );
    assert_eq!(client.join().unwrap(), 502);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "only the last one reached the provider"
    );
}

/// A stand-in Messages API that records each request's head and answers one
/// SSE stream.
fn anthropic_upstream() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let heads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = heads.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            if let Ok((target, headers, _)) = read_request(&mut stream, 1 << 20) {
                let mut head = target;
                for (name, value) in headers {
                    head.push_str(&format!("\n{name}: {value}"));
                }
                seen.lock().unwrap().push(head);
            }
            let body = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, heads)
}

fn anthropic_proxy(upstream: &str) -> ModelProxy {
    ModelProxy::bind(ProxyConfig {
        run_id: "run-7".into(),
        token: "run-7-token".into(),
        upstream: upstream.into(),
        backend: ProxyBackend::Anthropic,
        model: "claude-opus-4-8".into(),
        context_window: None,
        max_request_bytes: 1 << 20,
        max_response_bytes: 1 << 20,
        credential: Some(llm::UpstreamCredential::new("x-api-key", "sk-real")),
    })
    .unwrap()
}

/// The request shape the Claude Agent SDK's runtime sends after a tool call:
/// its system blocks and environment message, then the host's tool result.
fn messages_request(max_tokens: u64, extra: Option<Value>) -> Value {
    let mut result = vec![json!({"tool_use_id": "toolu_1", "type": "tool_result",
        "content": [{"type": "text", "text": AUTHORIZED}], "cache_control": {"type": "ephemeral"}})];
    result.extend(extra);
    json!({"model": "claude-opus-4-8", "max_tokens": max_tokens, "stream": true,
        "system": [{"type": "text", "text": "system text"}],
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "task text"}]},
            {"role": "system", "content": "# Environment"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1", "name": "mcp__cartograph__query_context", "input": {}}]},
            {"role": "user", "content": result}]})
}

/// Sends one request as the Claude lane's runtime would.
fn send_messages(
    base: String,
    target: &str,
    key: &str,
    body: Value,
) -> std::thread::JoinHandle<u16> {
    let target = target.to_owned();
    let key = key.to_owned();
    std::thread::spawn(move || {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("{base}{target}"))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "claude-code-20250219")
            .json(&body)
            .send()
            .unwrap()
            .status()
            .as_u16()
    })
}

// AC-1424 (ADR-0085 decision 7): the Claude lane's runtime reaches the
// Messages API only through the proxy, with the per-run token as its key; the
// proxy swaps in the real key and checks the same limits and texts.
#[test]
fn the_claude_lane_gets_the_real_key_only_at_the_proxy() {
    let (url, heads) = anthropic_upstream();
    let proxy = anthropic_proxy(&url);
    let client = send_messages(
        proxy.base_url().unwrap(),
        "/v1/messages?beta=true",
        "run-7-token",
        messages_request(2048, None),
    );
    let exchange = proxy
        .serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true)
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    assert_eq!(exchange.status, 200);
    let head = heads.lock().unwrap().pop().unwrap();
    assert!(head.starts_with("/v1/messages?beta=true\n"), "{head}");
    assert!(
        head.contains("\nx-api-key: sk-real"),
        "the real key goes upstream"
    );
    assert!(!head.contains("run-7-token"), "the per-run token does not");
    assert!(head.contains("\nanthropic-version: 2023-06-01"));
    assert!(head.contains("\nanthropic-beta: claude-code-20250219"));
    assert!(!format!("{:?}", proxy.config()).contains("sk-real"));

    let refused = |target: &str, key: &str, body: Value| {
        let proxy = anthropic_proxy(&url);
        let client = send_messages(proxy.base_url().unwrap(), target, key, body);
        let outcome = proxy.serve_one(&authorized(), &LIMITS, soon(), &never, &|_| true);
        (outcome.unwrap_err(), client.join().unwrap())
    };
    assert_eq!(
        refused("/v1/messages", "sk-real", messages_request(2048, None)),
        (ProxyRefusal::Token, 401)
    );
    assert_eq!(
        refused(
            "/v1/messages?other=1",
            "run-7-token",
            messages_request(2048, None)
        ),
        (ProxyRefusal::Path, 404)
    );
    assert_eq!(
        refused(
            "/v1/messages/count_tokens",
            "run-7-token",
            messages_request(2048, None)
        ),
        (ProxyRefusal::Path, 404)
    );
    assert_eq!(
        refused("/v1/messages", "run-7-token", messages_request(8192, None)),
        (ProxyRefusal::OutputTokens, 403)
    );
    let reminder = json!({"type": "text", "text": "<system-reminder>extra</system-reminder>"});
    assert_eq!(
        refused(
            "/v1/messages",
            "run-7-token",
            messages_request(2048, Some(reminder))
        ),
        (ProxyRefusal::Unauthorized, 403),
        "any text the host did not send is refused"
    );
    assert_eq!(
        heads.lock().unwrap().len(),
        0,
        "no refused request reached upstream"
    );
}

#[test]
fn messages_trailing_texts_are_one_per_block_after_the_last_answer() {
    let request = json!({"messages": [
        {"role": "user", "content": "first"},
        {"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "n", "input": {}}]},
        {"role": "system", "content": "environment"},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "a", "content": "one"},
            {"type": "tool_result", "tool_use_id": "b", "content": [{"type": "text", "text": "tw"}, {"type": "text", "text": "o"}]},
            {"type": "text", "text": "three"}]}]});
    assert_eq!(
        anthropic_trailing_texts(&request),
        Some(vec!["one".into(), "two".into(), "three".into()])
    );
    let image =
        json!({"messages": [{"role": "user", "content": [{"type": "image", "source": {}}]}]});
    assert_eq!(anthropic_trailing_texts(&image), None);
}

// AC-1420: GLM on vLLM answered a finish-only step with the finish as text and
// no tool call, which ends the runtime. A vLLM request that offers tools is
// forwarded with tool_choice "required"; a runtime's own choice stands, and
// the other backends' bodies pass through byte for byte.
#[test]
fn a_vllm_request_with_tools_must_call_one() {
    let offered = json!({"model": "glm", "tools": [{"type": "function",
        "function": {"name": "finish"}}], "messages": []});
    let forward = |backend, request: &Value| {
        let body = serde_json::to_vec(request).unwrap();
        let sent = forced_tool_choice(backend, request.clone(), body.clone(), 1 << 20).unwrap();
        (body, serde_json::from_slice::<Value>(&sent).unwrap(), sent)
    };
    let (_, sent, _) = forward(ProxyBackend::OpenAiCompatible, &offered);
    assert_eq!(sent["tool_choice"], "required");
    assert_eq!(sent["tools"], offered["tools"]);
    let mut chosen = offered.clone();
    chosen["tool_choice"] = json!("auto");
    let (body, _, sent) = forward(ProxyBackend::OpenAiCompatible, &chosen);
    assert_eq!(sent, body, "the runtime's choice stands");
    let none = json!({"model": "glm", "tools": [], "messages": []});
    let (body, _, sent) = forward(ProxyBackend::OpenAiCompatible, &none);
    assert_eq!(sent, body);
    for backend in [ProxyBackend::Ollama, ProxyBackend::Anthropic] {
        let (body, _, sent) = forward(backend, &offered);
        assert_eq!(sent, body);
    }
    // The added field may not carry a request past its limit.
    let body = serde_json::to_vec(&offered).unwrap();
    assert_eq!(
        forced_tool_choice(
            ProxyBackend::OpenAiCompatible,
            offered.clone(),
            body.clone(),
            body.len()
        ),
        Err(ProxyRefusal::Request)
    );
}
