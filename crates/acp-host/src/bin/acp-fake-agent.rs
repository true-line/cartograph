//! Scripted ACP agent for the dispatch tests. `ACP_FAKE_SCRIPT` picks the
//! script: `work` (default) exercises reads, writes, permissions and an
//! unoffered method, then ends its turn; `hang` waits for `session/cancel`;
//! `exit` exits mid-turn.

use serde_json::{Value, json};
use std::io::{BufRead, Write};

struct Agent {
    lines: std::io::Lines<std::io::StdinLock<'static>>,
    next_id: u64,
}

impl Agent {
    fn send(&self, message: Value) {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{message}").unwrap();
        out.flush().unwrap();
    }

    fn read(&mut self) -> Option<Value> {
        let line = self.lines.next()?.ok()?;
        serde_json::from_str(&line).ok()
    }

    /// Sends a request to the client and returns its whole response.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = 100 + self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.read().expect("client response");
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
            }
        }
    }

    fn say(&self, session: &str, text: &str) {
        self.send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}
        }}));
    }
}

fn outcome(response: &Value) -> String {
    match &response["result"]["outcome"] {
        Value::Object(outcome) if outcome["outcome"] == "selected" => {
            outcome["optionId"].as_str().unwrap_or("?").to_owned()
        }
        Value::Object(_) => "cancelled".into(),
        _ => format!("error {}", response["error"]["code"]),
    }
}

fn permission(agent: &mut Agent, session: &str, kind: &str, path: &str) -> String {
    let response = agent.request(
        "session/request_permission",
        json!({"sessionId": session,
               "toolCall": {"toolCallId": kind, "kind": kind, "locations": [{"path": path}]},
               "options": [
                   {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                   {"optionId": "always", "name": "Always", "kind": "allow_always"},
                   {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}),
    );
    outcome(&response)
}

fn work(agent: &mut Agent, session: &str, cwd: &str) -> String {
    let read = agent.request(
        "fs/read_text_file",
        json!({"sessionId": session, "path": format!("{cwd}/fixture.txt"), "line": 2, "limit": 1}),
    );
    let fixture = read["result"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_owned();
    let write = agent.request(
        "fs/write_text_file",
        json!({"sessionId": session, "path": format!("{cwd}/src/adapter.rs"),
               "content": format!("// covers {fixture}\n")}),
    );
    let escape = agent.request(
        "fs/write_text_file",
        json!({"sessionId": session, "path": format!("{cwd}/../escape.txt"), "content": "x"}),
    );
    let hook = agent.request(
        "fs/write_text_file",
        json!({"sessionId": session, "path": format!("{cwd}/.git/hooks/post-commit"), "content": "x"}),
    );
    let execute = permission(agent, session, "execute", &format!("{cwd}/fixture.txt"));
    let edit = permission(agent, session, "edit", &format!("{cwd}/src/adapter.rs"));
    let terminal = agent.request(
        "terminal/create",
        json!({"sessionId": session, "command": "true"}),
    );
    format!(
        "read={fixture};write={};escape={};hook={};execute={execute};edit={edit};terminal={}",
        if write.get("result").is_some() {
            "ok"
        } else {
            "error"
        },
        escape["error"]["code"],
        hook["error"]["code"],
        terminal["error"]["code"],
    )
}

fn main() {
    let script = std::env::var("ACP_FAKE_SCRIPT").unwrap_or_else(|_| "work".into());
    let mut agent = Agent {
        lines: std::io::stdin().lock().lines(),
        next_id: 0,
    };
    let mut cwd = String::new();
    while let Some(message) = agent.read() {
        let id = message.get("id").cloned();
        let params = &message["params"];
        match message["method"].as_str() {
            Some("initialize") => agent.send(json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})),
            Some("session/new") => {
                cwd = params["cwd"].as_str().unwrap_or("").to_owned();
                agent.send(json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "s1"}}));
            }
            Some("session/prompt") => {
                match script.as_str() {
                    "exit" => std::process::exit(3),
                    "hang" => {
                        while let Some(next) = agent.read() {
                            if next["method"] == "session/cancel" {
                                agent.send(json!({"jsonrpc": "2.0", "id": id,
                                              "result": {"stopReason": "cancelled"}}));
                            }
                        }
                        return;
                    }
                    _ => {
                        let report = work(&mut agent, "s1", &cwd.clone());
                        agent.say("s1", &report);
                        agent.send(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}));
                    }
                }
            }
            _ => {}
        }
    }
}
