// AC-1423 (ADR-0085): the host drives one tool-calling runtime per run over
// stdio; each bounded step is one model call through the run's proxy, and the
// model's next tool call comes back as the bounded loop's exact action bytes.
use super::*;
use crate::investigation::native_proxy::{CallLimits, ProxyConfig};
use crate::investigation::response_schema::CHOICE;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

const CONTEXT: &str = "context-v2:0000000000000000000000000000000000000000000000000000000000000000";
const MODEL: &str = "qwen3:30b-a3b";

/// What the fake model answers on one call.
#[derive(Clone)]
enum Answer {
    /// Tool calls, after a `pre_tool` hook the host must allow.
    Calls(usize),
    /// Text only: the runtime ends.
    Text,
}

struct FakeLink {
    to_runtime: Sender<Value>,
    from_runtime: Receiver<Value>,
}

impl RuntimeLink for FakeLink {
    fn send(&mut self, message: &Value) -> std::io::Result<()> {
        self.to_runtime
            .send(message.clone())
            .map_err(std::io::Error::other)
    }
    fn poll(&mut self) -> LinkPoll {
        match self.from_runtime.try_recv() {
            Ok(value) => LinkPoll::Message(value),
            Err(TryRecvError::Empty) => LinkPoll::Empty,
            Err(TryRecvError::Disconnected) => LinkPoll::Closed,
        }
    }
    fn recv(&mut self, deadline: Instant) -> LinkPoll {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.from_runtime.recv_timeout(left) {
            Ok(value) => LinkPoll::Message(value),
            Err(RecvTimeoutError::Timeout) => LinkPoll::Empty,
            Err(RecvTimeoutError::Disconnected) => LinkPoll::Closed,
        }
    }
}

fn query_arguments() -> Value {
    json!({"query": {"scope": {"type": "all"}, "kind": null, "labels": [],
        "max_facts": 12, "max_bytes": 16384, "cursor": null}})
}

/// A stand-in runtime: it posts each model call through the proxy the way Pi
/// does and plays `answers` in order. Tool results it receives go to `seen`.
fn runtime(answers: Vec<Answer>, seen: Arc<Mutex<Vec<Value>>>, runner: &'static str) -> FakeLink {
    let (to_runtime, inbox) = channel::<Value>();
    let (outbox, from_runtime) = channel::<Value>();
    std::thread::spawn(move || {
        outbox
            .send(
                json!({"type": "ready", "runtime": "fake-agent", "version": "1",
                "runners": {"pi": runner, "claude": "claude-agent-sdk@0"}}),
            )
            .unwrap();
        let Ok(start) = inbox.recv() else { return };
        assert_eq!(start["type"], "start");
        assert_eq!(start["backend"], "ollama");
        assert!(start["tools"][0]["name"].is_string());
        let url = format!("{}/api/chat", start["base_url"].as_str().unwrap());
        let token = start["token"].as_str().unwrap().to_owned();
        let mut messages = vec![
            json!({"role": "system", "content": start["system"]}),
            json!({"role": "user", "content": [{"type": "text", "text": start["prompt"]}]}),
        ];
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        for answer in answers {
            let body = json!({"model": MODEL, "options": {"num_ctx": 32768, "num_predict": start["max_output_tokens"]}, "messages": messages});
            let status = client
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .unwrap()
                .status();
            assert!(status.is_success(), "the proxy forwarded the call");
            let Answer::Calls(calls) = answer else {
                outbox
                    .send(json!({"type": "done", "reason": "stopped"}))
                    .unwrap();
                return;
            };
            messages.push(json!({"role": "assistant", "content": ""}));
            outbox
                .send(json!({"type": "hook", "hook_id": "hook-1", "point": "pre_tool"}))
                .unwrap();
            let Ok(hook) = inbox.recv() else { return };
            assert_eq!(hook["decision"], "allow");
            for call in 0..calls {
                outbox
                    .send(
                        json!({"type": "tool_call", "call_id": format!("call-{call}"),
                        "name": "query_context", "arguments": query_arguments()}),
                    )
                    .unwrap();
                let Ok(result) = inbox.recv() else { return };
                if result["type"] == "cancel" {
                    return;
                }
                messages.push(json!({"role": "tool", "content": result["content"]}));
                seen.lock().unwrap().push(result);
            }
        }
    });
    FakeLink {
        to_runtime,
        from_runtime,
    }
}

/// A stand-in Ollama that counts calls and reports usage.
fn upstream() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            let answer = format!(
                "{{\"model\":\"{MODEL}\",\"message\":{{\"role\":\"assistant\",\"content\":\"\"}},\"done\":true,\"prompt_eval_count\":11,\"eval_count\":7}}\n"
            );
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ncontent-length: {}\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (url, hits)
}

struct Harness {
    session: NativeSession<FakeLink>,
    hits: Arc<AtomicUsize>,
    spawns: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Value>>>,
}

/// A session whose n-th runtime start plays `scripts[n]`.
fn harness(scripts: Vec<Vec<Answer>>) -> Harness {
    let runners = vec![PI; scripts.len()];
    harness_with(scripts, runners)
}

const PI: &str = "pi-agent-core@0.73.1";

/// As [`harness`], with the n-th runtime reporting `runners[n]` as its Pi
/// runner.
fn harness_with(scripts: Vec<Vec<Answer>>, runners: Vec<&'static str>) -> Harness {
    let (url, hits) = upstream();
    let proxy = ModelProxy::bind(ProxyConfig {
        run_id: "run-9".into(),
        token: "run-9-token".into(),
        upstream: url,
        backend: ProxyBackend::Ollama,
        model: MODEL.into(),
        context_window: Some(32768),
        max_request_bytes: 1 << 20,
        max_response_bytes: 1 << 20,
    })
    .unwrap();
    let spawns = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (count, results) = (spawns.clone(), seen.clone());
    let config = SessionConfig {
        lane: RuntimeLane::Pi,
        max_turns: 12,
        budget_bytes: 1 << 20,
    };
    let session = NativeSession::new(
        proxy,
        config,
        Box::new(move || {
            let index = count.fetch_add(1, Ordering::SeqCst);
            Ok(runtime(
                scripts[index].clone(),
                results.clone(),
                runners[index],
            ))
        }),
    );
    Harness {
        session,
        hits,
        spawns,
        seen,
    }
}

fn payload(turn: u64) -> CompletionPayload {
    CompletionPayload {
        response_schema: Some(CHOICE.into()),
        system: "You are a bounded Cartograph investigation specialist.".into(),
        prompt: json!({"schema_version": 1, "context_id": CONTEXT, "question": "How does checkout work?",
            "turn_progress": {"turn": turn}})
        .to_string(),
        spans: vec![],
    }
}

const LIMITS: CallLimits = CallLimits {
    max_request_bytes: 1 << 20,
    max_response_bytes: 1 << 20,
    max_output_tokens: 2048,
};

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(20)
}

fn go() -> CallDirective {
    CallDirective::Continue
}

#[test]
fn a_step_is_one_model_call_that_returns_the_next_tool_call() {
    let mut h = harness(vec![vec![Answer::Calls(1), Answer::Calls(1)]]);
    let recorded = Mutex::new(Vec::new());
    let record = |bytes| {
        recorded.lock().unwrap().push(bytes);
        true
    };
    let first = h
        .session
        .step(&payload(1), &LIMITS, soon(), &go, &record)
        .unwrap();
    let action: Value = serde_json::from_str(&first.text).unwrap();
    assert_eq!(action["type"], "query_context");
    assert_eq!(action["context_id"], CONTEXT);
    assert_eq!(first.started, Some(StartReason::First));
    assert_eq!(first.response_model.as_deref(), Some(MODEL));
    assert_eq!(first.usage.as_ref().unwrap().input_tokens, Some(11));
    assert_eq!(first.usage.as_ref().unwrap().output_tokens, Some(7));
    assert_eq!(
        h.session.identity().unwrap(),
        &RuntimeIdentity {
            runtime: "fake-agent".into(),
            version: "1".into(),
            runner: PI.into(),
        },
        "the run names the runtime and the lane's runner it ran on"
    );
    let second = h
        .session
        .step(&payload(2), &LIMITS, soon(), &go, &record)
        .unwrap();
    assert_eq!(
        second.started, None,
        "the second step answers the last call"
    );
    assert_eq!(h.hits.load(Ordering::SeqCst), 2, "one model call per step");
    assert_eq!(h.spawns.load(Ordering::SeqCst), 1);
    assert_eq!(
        *recorded.lock().unwrap(),
        vec![first.exchange.request_bytes, second.exchange.request_bytes],
        "every forwarded request was recorded by size"
    );
    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["is_error"], false);
    assert!(
        seen[0]["content"]
            .as_str()
            .unwrap()
            .starts_with("{\"changed\":{\"turn_progress\""),
        "only what changed goes back"
    );
}

#[test]
fn a_second_call_in_one_answer_is_refused_and_carried_in_the_next_request() {
    let mut h = harness(vec![vec![Answer::Calls(2), Answer::Calls(1)]]);
    h.session
        .step(&payload(1), &LIMITS, soon(), &go, &|_| true)
        .unwrap();
    let second = h
        .session
        .step(&payload(2), &LIMITS, soon(), &go, &|_| true)
        .unwrap();
    assert_eq!(second.started, None);
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1]["is_error"], true);
    assert_eq!(seen[1]["content"], EXTRA_CALL);
}

#[test]
fn an_answer_without_a_tool_call_fails_the_step_and_the_next_one_restarts() {
    let mut h = harness(vec![vec![Answer::Text], vec![Answer::Calls(1)]]);
    assert_eq!(
        h.session.step(&payload(1), &LIMITS, soon(), &go, &|_| true),
        Err(answered(FailureCode::EmptyCompletion))
    );
    let next = h
        .session
        .step(&payload(2), &LIMITS, soon(), &go, &|_| true)
        .unwrap();
    assert_eq!(next.started, Some(StartReason::First));
    assert_eq!(h.spawns.load(Ordering::SeqCst), 2);
    assert_eq!(h.hits.load(Ordering::SeqCst), 2);
}

// AC-1423: a restart must report the identity the run started on; a
// different runner fails the step before anything is sent, and the run keeps
// its first identity.
#[test]
fn a_restart_on_a_different_runner_is_refused() {
    let mut h = harness_with(
        vec![vec![Answer::Text], vec![Answer::Calls(1)]],
        vec![PI, "pi-agent-core@0.74.0"],
    );
    assert_eq!(
        h.session.step(&payload(1), &LIMITS, soon(), &go, &|_| true),
        Err(answered(FailureCode::EmptyCompletion))
    );
    assert_eq!(
        h.session.step(&payload(2), &LIMITS, soon(), &go, &|_| true),
        Err(not_sent(FailureCode::Transport))
    );
    assert_eq!(h.spawns.load(Ordering::SeqCst), 2);
    assert_eq!(
        h.hits.load(Ordering::SeqCst),
        1,
        "nothing reached the model"
    );
    assert_eq!(h.session.identity().unwrap().runner, PI);
}

#[test]
fn a_cancelled_or_unrecorded_step_sends_nothing() {
    let mut h = harness(vec![vec![Answer::Calls(1)], vec![Answer::Calls(1)]]);
    assert_eq!(
        h.session.step(
            &payload(1),
            &LIMITS,
            soon(),
            &|| CallDirective::Cancelled,
            &|_| true
        ),
        Err(not_sent(FailureCode::Cancelled))
    );
    assert_eq!(h.spawns.load(Ordering::SeqCst), 0);
    let refused = h
        .session
        .step(&payload(1), &LIMITS, soon(), &go, &|_| false);
    assert_eq!(refused, Err(not_sent(FailureCode::EgressDenied)));
    assert_eq!(h.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn reported_usage_reads_ollama_and_openai_streams() {
    let ollama = b"{\"model\":\"m\",\"done\":false}\n{\"model\":\"m\",\"done\":true,\"prompt_eval_count\":3,\"eval_count\":4}\n";
    let (model, usage) = reported(ollama);
    assert_eq!(model.as_deref(), Some("m"));
    assert_eq!(usage.unwrap().output_tokens, Some(4));
    let sse = b"data: {\"model\":\"glm-5.3-flash\",\"choices\":[]}\n\ndata: {\"model\":\"glm-5.3-flash\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":6}}\n\ndata: [DONE]\n\n";
    let (model, usage) = reported(sse);
    assert_eq!(model.as_deref(), Some("glm-5.3-flash"));
    assert_eq!(usage.unwrap().input_tokens, Some(5));
}

#[cfg(unix)]
#[test]
fn a_process_link_speaks_one_json_object_per_line() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        r#"printf '{"type":"ready","runtime":"sh","version":"1","runners":{"pi":"pi-agent-core@0"}}\n'; read line; printf '%s\n' "$line"; printf 'not json\n'; sleep 30"#,
    ]);
    let mut link = ProcessLink::spawn(command).unwrap();
    assert_eq!(
        link.recv(soon()),
        LinkPoll::Message(json!({"type": "ready", "runtime": "sh", "version": "1",
            "runners": {"pi": "pi-agent-core@0"}}))
    );
    link.send(&json!({"type": "cancel"})).unwrap();
    assert_eq!(
        link.recv(soon()),
        LinkPoll::Message(json!({"type": "cancel"}))
    );
    assert_eq!(
        link.recv(soon()),
        LinkPoll::Closed,
        "a line that is not a JSON object ends the link"
    );
}

#[test]
fn control_is_checked_again_at_dispatch() {
    let recorded = Cell::new(false);
    let record = |_| {
        recorded.set(true);
        true
    };
    let stopped = Cell::new(None);
    assert!(!guard_dispatch(
        &|| CallDirective::OwnershipLost,
        &record,
        &stopped,
        10
    ));
    assert_eq!(stopped.get(), Some(not_sent(FailureCode::OwnershipLost)));
    assert!(!recorded.get(), "a lost run records and sends nothing");
    let stopped = Cell::new(None);
    assert!(guard_dispatch(&go, &record, &stopped, 10));
    assert!(recorded.get());
    assert_eq!(stopped.get(), None);
    // A run that stops while waiting for the request also sends nothing.
    let mut h = harness(vec![vec![Answer::Calls(1)]]);
    let calls = Cell::new(0);
    let lost = || {
        calls.set(calls.get() + 1);
        if calls.get() > 1 {
            CallDirective::Cancelled
        } else {
            CallDirective::Continue
        }
    };
    assert_eq!(
        h.session
            .step(&payload(1), &LIMITS, soon(), &lost, &|_| true),
        Err(not_sent(FailureCode::Cancelled))
    );
    assert_eq!(h.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn the_runtime_asks_for_the_steps_output_limit() {
    let mut h = harness(vec![vec![Answer::Calls(1)]]);
    let tight = CallLimits {
        max_output_tokens: 512,
        ..LIMITS
    };
    h.session
        .step(&payload(1), &tight, soon(), &go, &|_| true)
        .unwrap();
    assert_eq!(h.hits.load(Ordering::SeqCst), 1);
}
