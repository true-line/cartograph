//! One ACP v1 session over newline-delimited JSON-RPC 2.0 on stdio.

use crate::policy::{PathPolicy, PermissionDecision, decide_permission};
use crate::{AgentCommand, Limits, Refusal, StopReason};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// ACP protocol version this host speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// JSON-RPC: method not found.
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC: invalid params.
const INVALID_PARAMS: i64 = -32602;
/// Server error range: refused by the host's policy.
const REFUSED: i64 = -32000;
/// Longest line read from the agent.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) struct SessionOutcome {
    pub stop: StopReason,
    pub transcript: Vec<Value>,
    pub transcript_truncated: bool,
    pub refused: Vec<Refusal>,
}

enum Incoming {
    Line(String),
    Eof,
}

struct Session<'a> {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Incoming>,
    policy: &'a PathPolicy,
    limits: &'a Limits,
    session_id: Option<String>,
    deadline: Instant,
    cancelled: bool,
    out: SessionOutcome,
}

/// Why waiting for a response ended without one.
enum Ended {
    Stop(StopReason),
}

impl Session<'_> {
    fn send(&mut self, message: Value) -> Result<(), Ended> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(Ended::Stop(StopReason::AgentExited))?;
        let mut line = message.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| Ended::Stop(StopReason::AgentExited))
    }

    fn respond(&mut self, id: Value, result: Result<Value, (i64, String)>) -> Result<(), Ended> {
        let message = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
            }
        };
        self.send(message)
    }

    fn refuse(&mut self, method: &str, detail: String) -> (i64, String) {
        self.out.refused.push(Refusal {
            method: method.to_owned(),
            detail: detail.clone(),
        });
        (REFUSED, detail)
    }

    fn record(&mut self, update: Value) {
        if self.out.transcript.len() < self.limits.max_updates {
            self.out.transcript.push(update);
        } else {
            self.out.transcript_truncated = true;
        }
    }

    /// Sends a request and serves the agent until its response arrives.
    fn call(&mut self, id: u64, method: &str, params: Value) -> Result<Value, Ended> {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let now = Instant::now();
            let wait = self.deadline.saturating_duration_since(now);
            let line = match self.lines.recv_timeout(wait) {
                Ok(Incoming::Line(line)) => line,
                Ok(Incoming::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(Ended::Stop(StopReason::AgentExited));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self.cancelled {
                        return Err(Ended::Stop(StopReason::TimedOut));
                    }
                    self.cancelled = true;
                    self.deadline = Instant::now() + self.limits.cancel_grace;
                    if let Some(session_id) = self.session_id.clone() {
                        self.send(json!({"jsonrpc": "2.0", "method": "session/cancel",
                                         "params": {"sessionId": session_id}}))?;
                    } else {
                        return Err(Ended::Stop(StopReason::TimedOut));
                    }
                    continue;
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                return Err(Ended::Stop(StopReason::ProtocolError(
                    "agent wrote a line that is not JSON".into(),
                )));
            };
            let incoming = message
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match (incoming, message.get("id").cloned()) {
                (Some(method), Some(request_id)) => self.serve(&method, request_id, &message)?,
                (Some(method), None) => {
                    if method == "session/update" {
                        self.record(message.get("params").cloned().unwrap_or(Value::Null));
                    }
                }
                (None, Some(response_id)) if response_id == json!(id) => {
                    if let Some(error) = message.get("error") {
                        return Err(Ended::Stop(StopReason::ProtocolError(format!(
                            "{method} failed: {error}"
                        ))));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                _ => {}
            }
        }
    }

    fn serve(&mut self, method: &str, id: Value, message: &Value) -> Result<(), Ended> {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "session/request_permission" => Ok(self.permission(&params)),
            "fs/read_text_file" => self.read_text_file(&params),
            "fs/write_text_file" => self.write_text_file(&params),
            _ => {
                let detail = format!("method {method} is not offered");
                self.out.refused.push(Refusal {
                    method: method.to_owned(),
                    detail: detail.clone(),
                });
                Err((METHOD_NOT_FOUND, detail))
            }
        };
        self.respond(id, result)
    }

    fn permission(&mut self, params: &Value) -> Value {
        if self.cancelled {
            return json!({"outcome": {"outcome": "cancelled"}});
        }
        match decide_permission(params, self.policy) {
            PermissionDecision::Allow(option) => {
                json!({"outcome": {"outcome": "selected", "optionId": option}})
            }
            PermissionDecision::Reject(option, reason) => {
                self.refuse("session/request_permission", reason);
                json!({"outcome": {"outcome": "selected", "optionId": option}})
            }
            PermissionDecision::Cancel(reason) => {
                self.refuse("session/request_permission", reason);
                json!({"outcome": {"outcome": "cancelled"}})
            }
        }
    }

    fn path_param(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<std::path::PathBuf, (i64, String)> {
        let Some(path) = params.get("path").and_then(Value::as_str) else {
            return Err((INVALID_PARAMS, "path is required".into()));
        };
        let policy = self.policy;
        policy
            .resolve(Path::new(path))
            .map_err(|refusal| self.refuse(method, format!("{path}: {refusal}")))
    }

    fn read_text_file(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let path = self.path_param("fs/read_text_file", params)?;
        let size = std::fs::metadata(&path)
            .map_err(|error| (REFUSED, error.to_string()))?
            .len();
        if size > self.limits.max_file_bytes as u64 {
            return Err(self.refuse(
                "fs/read_text_file",
                format!("{} is {size} bytes, over the limit", path.display()),
            ));
        }
        let text = std::fs::read_to_string(&path).map_err(|error| (REFUSED, error.to_string()))?;
        let line = params
            .get("line")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let limit = params.get("limit").and_then(Value::as_u64);
        let content = if line == 1 && limit.is_none() {
            text
        } else {
            let skip = usize::try_from(line - 1).unwrap_or(usize::MAX);
            let take = limit.map_or(usize::MAX, |n| usize::try_from(n).unwrap_or(usize::MAX));
            text.split_inclusive('\n').skip(skip).take(take).collect()
        };
        Ok(json!({"content": content}))
    }

    fn write_text_file(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let Some(content) = params.get("content").and_then(Value::as_str) else {
            return Err((INVALID_PARAMS, "content is required".into()));
        };
        if content.len() > self.limits.max_file_bytes {
            return Err(self.refuse(
                "fs/write_text_file",
                format!("{} bytes is over the limit", content.len()),
            ));
        }
        let content = content.to_owned();
        let path = self.path_param("fs/write_text_file", params)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| (REFUSED, error.to_string()))?;
        }
        std::fs::write(&path, content).map_err(|error| (REFUSED, error.to_string()))?;
        Ok(json!({}))
    }

    fn turn(&mut self, cwd: &Path, prompt: &str) -> Result<StopReason, Ended> {
        let init = self.call(
            0,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}, "terminal": false},
                "clientInfo": {"name": "cartograph", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        let version = init.get("protocolVersion").and_then(Value::as_u64);
        if version != Some(PROTOCOL_VERSION) {
            return Ok(StopReason::ProtocolError(format!(
                "agent speaks protocol version {version:?}, not {PROTOCOL_VERSION}"
            )));
        }
        let session = self.call(
            1,
            "session/new",
            json!({"cwd": cwd.to_string_lossy(), "mcpServers": []}),
        )?;
        let Some(session_id) = session.get("sessionId").and_then(Value::as_str) else {
            return Ok(StopReason::ProtocolError(
                "session/new returned no sessionId".into(),
            ));
        };
        self.session_id = Some(session_id.to_owned());
        let reply = self.call(
            2,
            "session/prompt",
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": prompt}]}),
        )?;
        let stop = match reply.get("stopReason").and_then(Value::as_str) {
            Some("end_turn") => StopReason::EndTurn,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("max_turn_requests") => StopReason::MaxTurnRequests,
            Some("refusal") => StopReason::Refusal,
            Some("cancelled") => StopReason::Cancelled,
            other => StopReason::ProtocolError(format!("unknown stopReason {other:?}")),
        };
        Ok(stop)
    }
}

fn spawn_reader(stdout: impl std::io::Read + Send + 'static) -> Receiver<Incoming> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            let read = reader
                .by_ref()
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut line);
            match read {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > MAX_LINE_BYTES => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line).trim().to_owned();
                    if !text.is_empty() && tx.send(Incoming::Line(text)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Incoming::Eof);
    });
    rx
}

/// Runs one prompt turn of `agent` in `cwd` and returns how it ended.
pub(crate) fn run_session(
    agent: &AgentCommand,
    cwd: &Path,
    prompt: &str,
    policy: &PathPolicy,
    limits: &Limits,
) -> std::io::Result<SessionOutcome> {
    let mut child = Command::new(&agent.program)
        .args(&agent.args)
        .envs(agent.env.iter().map(|(key, value)| (key, value)))
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take();
    let lines = spawn_reader(child.stdout.take().expect("stdout is piped"));
    let mut session = Session {
        child,
        stdin,
        lines,
        policy,
        limits,
        session_id: None,
        deadline: Instant::now() + limits.turn,
        cancelled: false,
        out: SessionOutcome {
            stop: StopReason::AgentExited,
            transcript: Vec::new(),
            transcript_truncated: false,
            refused: Vec::new(),
        },
    };
    let stop = match session.turn(cwd, prompt) {
        Ok(StopReason::Cancelled) if session.cancelled => StopReason::TimedOut,
        Ok(stop) | Err(Ended::Stop(stop)) => stop,
    };
    session.stdin = None;
    let exit_by = Instant::now() + Duration::from_millis(500);
    while session.child.try_wait().ok().flatten().is_none() && Instant::now() < exit_by {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = session.child.kill();
    let _ = session.child.wait();
    session.out.stop = stop;
    Ok(session.out)
}
