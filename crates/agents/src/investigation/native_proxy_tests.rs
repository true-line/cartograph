// AC-1422 (ADR-0085): the loopback model proxy forwards one authorized request
// per step and refuses everything else before it reaches the provider.
use super::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    })
    .unwrap()
}

fn request(model: &str, num_ctx: u64, last: &str) -> Value {
    json!({"model": model, "stream": true, "options": {"num_ctx": num_ctx},
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
        .serve_one(&authorized(), soon(), &never, &|bytes| {
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
        let outcome = proxy.serve_one(&authorized(), soon(), &never, &|_| {
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
        proxy.serve_one(&authorized(), soon(), &never, &|_| false),
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
        proxy.serve_one(&authorized(), deadline, &never, &|_| true),
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
    })
    .unwrap();
    let body =
        json!({"model": "glm-5.3-flash", "messages": [{"role": "user", "content": AUTHORIZED}]});
    let client = send(proxy.base_url().unwrap(), "/v1/chat/completions", "t", body);
    proxy
        .serve_one(&authorized(), soon(), &never, &|_| true)
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
    let body = |messages: &Value| json!({"model": "qwen3:30b-a3b", "options": {"num_ctx": 32768}, "messages": messages});
    let both = vec![AUTHORIZED.to_owned(), extra.clone()];
    let proxy = proxy(&url);
    let client = send(
        proxy.base_url().unwrap(),
        "/api/chat",
        "run-7-token",
        body(&messages),
    );
    assert_eq!(
        proxy.serve_one(&authorized(), soon(), &never, &|_| true),
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
    proxy.serve_one(&both, soon(), &never, &|_| true).unwrap();
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
        .serve_one(&["start".to_owned()], soon(), &never, &|_| true)
        .unwrap();
    assert_eq!(client.join().unwrap(), 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn an_interrupt_ends_the_wait_before_any_request() {
    let (url, _) = upstream();
    let proxy = proxy(&url);
    assert_eq!(
        proxy.serve_one(&authorized(), soon(), &|| true, &|_| true),
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
        proxy.serve_one(&authorized(), soon(), &never, &|_| true),
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
        proxy.serve_one(&authorized(), soon(), &never, &|_| true),
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
        proxy.serve_one(&authorized(), soon(), &never, &|_| true),
        Err(ProxyRefusal::Request),
        "an endless request line is refused"
    );
    flood.join().unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}
