//! One tool-calling run's runtime session, and the bounded provider that
//! drives it (ADR-0085, AC-1423).
//!
//! Each bounded step is one model call. The session hands the step's
//! authorized payload to the `cartograph-agent` runtime as the bridge decides
//! (a start, or the result of the model's last tool call), serves the one
//! model request that follows through the run's loopback proxy, and returns
//! the model's next tool call as the bounded loop's exact action bytes. The
//! worker, its admission and its ledger do not change.
//!
//! A failed step leaves no call pending, so the next step restarts the
//! runtime from the full authorized payload.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use llm::bounded::{
    AuthorizedBoundedCompletion, BoundedCallError, BoundedCompletion, CallDirective,
    CompletionControl, FailureCode, InvocationOutcome, ProviderProfile, ReportedUsage,
};
use llm::{CompletionPayload, LlmProvider};
use serde_json::{Value, json};

use super::native_bridge::{BridgeConversation, BridgeTurn, StartReason};
use super::native_proxy::{ModelProxy, ProxyBackend, ProxyExchange, ProxyRefusal};

/// The tool result sent for a second tool call in one model answer. The
/// bounded loop takes one action per step, so only the first call runs.
pub const EXTRA_CALL: &str = "Only one tool call per turn is run; this call was not run. Read the previous result, then call again if you still need it.";

/// Largest sidecar line read.
const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;

/// What one poll of the runtime gave.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkPoll {
    /// One sidecar message.
    Message(Value),
    /// Nothing yet.
    Empty,
    /// The runtime closed its output or broke the protocol.
    Closed,
}

/// The stdio channel to one runtime process.
pub trait RuntimeLink: Send {
    /// Sends one host message.
    fn send(&mut self, message: &Value) -> std::io::Result<()>;
    /// The next sidecar message if one is waiting.
    fn poll(&mut self) -> LinkPoll;
    /// The next sidecar message, waiting until `deadline`.
    fn recv(&mut self, deadline: Instant) -> LinkPoll;
}

/// A `cartograph-agent` child process, one per runtime start.
pub struct ProcessLink {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
}

impl ProcessLink {
    /// Starts `command` with piped stdio. Stderr is discarded.
    pub fn spawn(mut command: Command) -> std::io::Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(std::io::Error::other("runtime stdio unavailable"));
        };
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match (&mut reader)
                    .take(MAX_LINE_BYTES + 1)
                    .read_until(b'\n', &mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() as u64 > MAX_LINE_BYTES => break,
                    Ok(_) => {}
                }
                // Anything but one JSON object per line ends the link.
                let Ok(value @ Value::Object(_)) = serde_json::from_slice::<Value>(&line) else {
                    break;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
        })
    }
}

impl RuntimeLink for ProcessLink {
    fn send(&mut self, message: &Value) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.stdin.write_all(&line)?;
        self.stdin.flush()
    }

    fn poll(&mut self) -> LinkPoll {
        match self.lines.try_recv() {
            Ok(value) => LinkPoll::Message(value),
            Err(TryRecvError::Empty) => LinkPoll::Empty,
            Err(TryRecvError::Disconnected) => LinkPoll::Closed,
        }
    }

    fn recv(&mut self, deadline: Instant) -> LinkPoll {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(left) {
            Ok(value) => LinkPoll::Message(value),
            Err(RecvTimeoutError::Timeout) => LinkPoll::Empty,
            Err(RecvTimeoutError::Disconnected) => LinkPoll::Closed,
        }
    }
}

impl Drop for ProcessLink {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The harness that drives the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLane {
    /// `pi-agent-core`, for local and open models.
    Pi,
    /// The Claude Agent SDK.
    Claude,
}

/// Fixed settings for one run's runtime.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// The harness.
    pub lane: RuntimeLane,
    /// Model turns the runtime may take before it stops on its own.
    pub max_turns: u32,
    /// Output tokens per model call.
    pub max_output_tokens: Option<u32>,
    /// Conversation byte budget before the bridge restarts the runtime.
    pub budget_bytes: usize,
}

/// The runtime's self-reported identity, from its `ready` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeIdentity {
    /// Runtime name.
    pub runtime: String,
    /// Runtime protocol version.
    pub version: String,
}

/// One step's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutput {
    /// The bounded loop's exact action bytes.
    pub text: String,
    /// The forwarded model request and its response.
    pub exchange: ProxyExchange,
    /// The model the provider reported, if any.
    pub response_model: Option<String>,
    /// Provider-reported usage, if any.
    pub usage: Option<ReportedUsage>,
    /// Why the runtime (re)started this step, if it did.
    pub started: Option<StartReason>,
}

type Spawn<L> = Box<dyn FnMut() -> std::io::Result<L> + Send>;

/// One run's runtime, its conversation and its proxy.
pub struct NativeSession<L: RuntimeLink> {
    spawn: Spawn<L>,
    link: Option<L>,
    identity: Option<RuntimeIdentity>,
    bridge: BridgeConversation,
    proxy: ModelProxy,
    config: SessionConfig,
    pending_call: Option<String>,
}

fn failure(code: FailureCode, outcome: InvocationOutcome) -> BoundedCallError {
    BoundedCallError { code, outcome }
}

fn not_sent(code: FailureCode) -> BoundedCallError {
    failure(code, InvocationOutcome::NotDispatched)
}

fn answered(code: FailureCode) -> BoundedCallError {
    failure(code, InvocationOutcome::ResponseReceived)
}

fn directive_error(directive: CallDirective) -> Option<BoundedCallError> {
    match directive {
        CallDirective::Continue => None,
        CallDirective::Cancelled => Some(not_sent(FailureCode::Cancelled)),
        CallDirective::OwnershipLost => Some(not_sent(FailureCode::OwnershipLost)),
    }
}

fn kind(message: &Value) -> &str {
    message.get("type").and_then(Value::as_str).unwrap_or("")
}

fn allow(hook: &Value) -> Value {
    json!({"type": "hook_result", "hook_id": hook.get("hook_id").cloned().unwrap_or(Value::Null),
        "decision": "allow"})
}

impl<L: RuntimeLink> NativeSession<L> {
    /// A session that starts runtimes with `spawn` and serves their model
    /// requests through `proxy`.
    pub fn new(proxy: ModelProxy, config: SessionConfig, spawn: Spawn<L>) -> Self {
        Self {
            spawn,
            link: None,
            identity: None,
            bridge: BridgeConversation::new(),
            proxy,
            config,
            pending_call: None,
        }
    }

    /// The identity the current runtime reported.
    pub fn identity(&self) -> Option<&RuntimeIdentity> {
        self.identity.as_ref()
    }

    /// The model id this session serves.
    pub fn model(&self) -> &str {
        &self.proxy.config().model
    }

    /// Runs one bounded step: one model call for `payload`, which the
    /// firewall authorized for this step.
    pub fn step(
        &mut self,
        payload: &CompletionPayload,
        deadline: Instant,
        check: &dyn Fn() -> CallDirective,
        dispatching: &dyn Fn(u64) -> bool,
    ) -> Result<StepOutput, BoundedCallError> {
        if let Some(error) = directive_error(check()) {
            return Err(error);
        }
        if self.pending_call.is_none() {
            // No call to answer: the next turn is a start.
            self.bridge = BridgeConversation::new();
        }
        let turn = self
            .bridge
            .next_turn(payload, self.config.budget_bytes)
            .map_err(|_| not_sent(FailureCode::InvalidInput))?;
        let outcome = self.drive(turn, deadline, check, dispatching);
        if outcome.is_err() {
            self.pending_call = None;
        }
        outcome
    }

    fn drive(
        &mut self,
        turn: BridgeTurn,
        deadline: Instant,
        check: &dyn Fn() -> CallDirective,
        dispatching: &dyn Fn(u64) -> bool,
    ) -> Result<StepOutput, BoundedCallError> {
        let (mut authorized, started) = match turn {
            BridgeTurn::Start {
                system,
                prompt,
                tools,
                reason,
            } => {
                self.start(&system, &prompt, &tools, deadline)?;
                (vec![prompt], Some(reason))
            }
            BridgeTurn::ToolResult { content } => {
                let call_id = self
                    .pending_call
                    .take()
                    .ok_or(not_sent(FailureCode::InvalidInput))?;
                self.send(
                    &json!({"type": "tool_result", "call_id": call_id, "content": content,
                    "is_error": false, "terminal": false}),
                )?;
                (vec![content], None)
            }
        };
        let exchange = self.serve(&mut authorized, deadline, check, dispatching)?;
        if !(200..300).contains(&exchange.status) {
            return Err(answered(FailureCode::HttpStatus));
        }
        let (response_model, usage) = reported(&exchange.body);
        let link = self.link.as_mut().ok_or(answered(FailureCode::Transport))?;
        loop {
            let message = match link.recv(deadline) {
                LinkPoll::Message(message) => message,
                LinkPoll::Empty => return Err(answered(FailureCode::DeadlineExceeded)),
                LinkPoll::Closed => return Err(answered(FailureCode::Transport)),
            };
            match kind(&message) {
                "hook" => link
                    .send(&allow(&message))
                    .map_err(|_| answered(FailureCode::Transport))?,
                "tool_call" => {
                    let (Some(call_id), Some(name)) = (
                        message.get("call_id").and_then(Value::as_str),
                        message.get("name").and_then(Value::as_str),
                    ) else {
                        return Err(answered(FailureCode::InvalidResponse));
                    };
                    let arguments = message.get("arguments").cloned().unwrap_or(Value::Null);
                    self.pending_call = Some(call_id.to_owned());
                    self.bridge
                        .record_model_output(name.len() + arguments.to_string().len());
                    let text = self
                        .bridge
                        .action_text(name, &arguments)
                        .map_err(|_| answered(FailureCode::InvalidResponse))?;
                    return Ok(StepOutput {
                        text,
                        exchange,
                        response_model,
                        usage,
                        started,
                    });
                }
                // The model answered without a tool call and the runtime ended.
                "done" => return Err(answered(FailureCode::EmptyCompletion)),
                _ => return Err(answered(FailureCode::InvalidResponse)),
            }
        }
    }

    /// Stops any runtime, starts a new one and sends it the run's start.
    fn start(
        &mut self,
        system: &str,
        prompt: &str,
        tools: &[super::native_tools::ToolDefinition],
        deadline: Instant,
    ) -> Result<(), BoundedCallError> {
        self.stop();
        let mut link = (self.spawn)().map_err(|_| not_sent(FailureCode::Transport))?;
        let ready = match link.recv(deadline) {
            LinkPoll::Message(message) if kind(&message) == "ready" => message,
            LinkPoll::Empty => return Err(not_sent(FailureCode::DeadlineExceeded)),
            _ => return Err(not_sent(FailureCode::Transport)),
        };
        let field = |name: &str| {
            ready
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        self.identity = Some(RuntimeIdentity {
            runtime: field("runtime"),
            version: field("version"),
        });
        let proxy = self.proxy.config();
        let base_url = self
            .proxy
            .base_url()
            .map_err(|_| not_sent(FailureCode::Transport))?;
        let mut start = json!({
            "type": "start",
            "run_id": proxy.run_id,
            "lane": match self.config.lane { RuntimeLane::Pi => "pi", RuntimeLane::Claude => "claude" },
            "model": proxy.model,
            "base_url": base_url,
            "token": proxy.token,
            "system": system,
            "prompt": prompt,
            "tools": tools,
            "hooks": [],
            "max_turns": self.config.max_turns,
        });
        if let Some(window) = proxy.context_window {
            start["context_window"] = json!(window);
        }
        if let Some(tokens) = self.config.max_output_tokens {
            start["max_output_tokens"] = json!(tokens);
        }
        if self.config.lane == RuntimeLane::Pi {
            start["backend"] = json!(match proxy.backend {
                ProxyBackend::Ollama => "ollama",
                ProxyBackend::OpenAiCompatible => "vllm",
            });
        }
        link.send(&start)
            .map_err(|_| not_sent(FailureCode::Transport))?;
        self.link = Some(link);
        Ok(())
    }

    /// Serves the step's one model request. Hooks are allowed and a second
    /// tool call in the same answer is refused with [`EXTRA_CALL`], which then
    /// belongs to the texts the request must carry.
    fn serve(
        &mut self,
        authorized: &mut Vec<String>,
        deadline: Instant,
        check: &dyn Fn() -> CallDirective,
        dispatching: &dyn Fn(u64) -> bool,
    ) -> Result<ProxyExchange, BoundedCallError> {
        let link = self.link.as_mut().ok_or(not_sent(FailureCode::Transport))?;
        loop {
            let (result, polled) = {
                let link = RefCell::new(&mut *link);
                let polled = RefCell::new(None);
                let interrupt = || {
                    if check() != CallDirective::Continue {
                        return true;
                    }
                    match link.borrow_mut().poll() {
                        LinkPoll::Empty => false,
                        other => {
                            *polled.borrow_mut() = Some(other);
                            true
                        }
                    }
                };
                let result = self
                    .proxy
                    .serve_one(authorized, deadline, &interrupt, dispatching);
                (result, polled.into_inner())
            };
            match result {
                Ok(exchange) => return Ok(exchange),
                Err(ProxyRefusal::Interrupted) => {}
                Err(refusal) => return Err(refused(refusal)),
            }
            if let Some(error) = directive_error(check()) {
                return Err(error);
            }
            let message = match polled {
                Some(LinkPoll::Message(message)) => message,
                _ => return Err(not_sent(FailureCode::Transport)),
            };
            match kind(&message) {
                "hook" => link
                    .send(&allow(&message))
                    .map_err(|_| not_sent(FailureCode::Transport))?,
                "tool_call" => {
                    link.send(&json!({"type": "tool_result",
                        "call_id": message.get("call_id").cloned().unwrap_or(Value::Null),
                        "content": EXTRA_CALL, "is_error": true, "terminal": false}))
                        .map_err(|_| not_sent(FailureCode::Transport))?;
                    authorized.push(EXTRA_CALL.to_owned());
                }
                _ => return Err(not_sent(FailureCode::IncompleteResponse)),
            }
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), BoundedCallError> {
        self.link
            .as_mut()
            .ok_or(not_sent(FailureCode::Transport))?
            .send(message)
            .map_err(|_| not_sent(FailureCode::Transport))
    }

    /// Cancels and drops the current runtime, if any.
    fn stop(&mut self) {
        if let Some(mut link) = self.link.take() {
            let _ = link.send(&json!({"type": "cancel"}));
        }
        self.pending_call = None;
    }
}

impl<L: RuntimeLink> Drop for NativeSession<L> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn refused(refusal: ProxyRefusal) -> BoundedCallError {
    match refusal {
        ProxyRefusal::Deadline => not_sent(FailureCode::DeadlineExceeded),
        ProxyRefusal::NotRecorded => not_sent(FailureCode::EgressDenied),
        // Recorded and handed to the provider, so it may have run.
        ProxyRefusal::Upstream => failure(FailureCode::Transport, InvocationOutcome::Unknown),
        ProxyRefusal::Interrupted => not_sent(FailureCode::Cancelled),
        // The runtime asked for something this step did not authorize.
        ProxyRefusal::Path
        | ProxyRefusal::Token
        | ProxyRefusal::Request
        | ProxyRefusal::Model
        | ProxyRefusal::ContextWindow
        | ProxyRefusal::Unauthorized => not_sent(FailureCode::InvalidResponse),
    }
}

/// The model id and usage a provider reported in a streamed or whole body:
/// Ollama NDJSON (`prompt_eval_count`, `eval_count`) or OpenAI-compatible
/// SSE / JSON (`usage.prompt_tokens`, `usage.completion_tokens`).
fn reported(body: &[u8]) -> (Option<String>, Option<ReportedUsage>) {
    let text = String::from_utf8_lossy(body);
    let mut model = None;
    let mut usage = None;
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("data:").map_or(line, str::trim);
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(name) = event.get("model").and_then(Value::as_str) {
            model = Some(name.to_owned());
        }
        let counts = event
            .get("usage")
            .filter(|usage| usage.is_object())
            .map(|usage| (usage.get("prompt_tokens"), usage.get("completion_tokens")))
            .or_else(|| {
                (event.get("done") == Some(&Value::Bool(true)))
                    .then(|| (event.get("prompt_eval_count"), event.get("eval_count")))
            });
        if let Some((input, output)) = counts {
            usage = Some(ReportedUsage {
                input_tokens: input.and_then(Value::as_u64),
                output_tokens: output.and_then(Value::as_u64),
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            });
        }
    }
    (model, usage)
}

/// A bounded provider whose steps run on a tool-calling runtime. Identity,
/// profile and embeddings are the wrapped provider's, so authorization and
/// the run record are unchanged; only the transport differs.
pub struct NativeToolProvider<L: RuntimeLink> {
    inner: Arc<dyn LlmProvider>,
    session: Mutex<NativeSession<L>>,
}

impl<L: RuntimeLink> NativeToolProvider<L> {
    /// Wraps `inner`; its bounded profile must request the session's model.
    pub fn new(
        inner: Arc<dyn LlmProvider>,
        session: NativeSession<L>,
    ) -> Result<Self, BoundedCallError> {
        let profile = inner.bounded_profile()?;
        if profile.requested_model != session.model() {
            return Err(not_sent(FailureCode::InvalidProfile));
        }
        Ok(Self {
            inner,
            session: Mutex::new(session),
        })
    }

    /// The identity the current runtime reported.
    pub fn runtime_identity(&self) -> Option<RuntimeIdentity> {
        self.session.lock().ok()?.identity().cloned()
    }
}

impl<L: RuntimeLink> LlmProvider for NativeToolProvider<L> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn locality(&self) -> llm::Locality {
        self.inner.locality()
    }

    fn capabilities(&self) -> llm::ProviderCaps {
        self.inner.capabilities()
    }

    fn embed(&self, batch: &[String]) -> Result<Vec<llm::Embedding>, llm::ProviderError> {
        self.inner.embed(batch)
    }

    fn embedding_cache_identity(
        &self,
    ) -> Result<Option<llm::EmbeddingIdentity>, llm::ProviderError> {
        self.inner.embedding_cache_identity()
    }

    fn embedding_model_id(&self) -> String {
        self.inner.embedding_model_id()
    }

    fn completion_model_id(&self) -> String {
        self.inner.completion_model_id()
    }

    fn supports_bounded_response_schema(&self) -> bool {
        self.inner.supports_bounded_response_schema()
    }

    fn bounded_profile(&self) -> Result<ProviderProfile, BoundedCallError> {
        self.inner.bounded_profile()
    }

    fn complete_bounded(
        &self,
        request: &AuthorizedBoundedCompletion,
        control: &CompletionControl<'_>,
    ) -> Result<BoundedCompletion, BoundedCallError> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| not_sent(FailureCode::Transport))?;
        let dispatching = |bytes| control.dispatching.is_none_or(|record| record(bytes));
        let output = session.step(
            request.payload(),
            control.deadline,
            control.check,
            &dispatching,
        )?;
        if output.text.len() as u64 > request.limits().values().output_text_bytes {
            return Err(answered(FailureCode::OutputTooLarge));
        }
        Ok(BoundedCompletion {
            text: output.text,
            requested_model: request.profile().requested_model.clone(),
            response_model: output.response_model,
            provider_request_id: None,
            usage: output.usage,
            request_bytes: output.exchange.request_bytes,
            response_bytes: output.exchange.response_bytes,
        })
    }
}

#[cfg(test)]
#[path = "native_session_tests.rs"]
mod tests;
