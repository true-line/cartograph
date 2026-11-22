//! Protocol admission only: no server, authentication, schemas or read handlers.

use std::{collections::BTreeSet, num::NonZeroUsize};

use crate::tools::ToolIdentity;
use serde_json::{Map, Value, json};

/// The sole protocol version understood by this admission layer.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

/// Closed vocabulary of possible reads; construction does not implement them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadTool {
    /// Describe available context capabilities.
    Capabilities,
    /// Discover bounded context handles.
    Discover,
    /// Read one selected fact under a scoped session basis.
    ReadFact,
    /// List official artifacts.
    Artifacts,
    /// Read a selected artifact.
    ReadArtifact,
    /// Describe or read retained evidence.
    Evidence,
    /// Select one bounded page of facts: all, one exact node, or a node's
    /// induced neighborhood (AC-1809).
    Query,
}

impl ReadTool {
    /// Stable external tool name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Capabilities => "cartograph_capabilities",
            Self::Discover => "cartograph_discover",
            Self::ReadFact => "cartograph_read_fact",
            Self::Artifacts => "cartograph_artifacts",
            Self::ReadArtifact => "cartograph_read_artifact",
            Self::Evidence => "cartograph_evidence",
            Self::Query => "cartograph_query",
        }
    }
}

/// Why the owner must close the transport without dispatching more messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A request reused an ID from this session.
    ReusedId,
    /// Retaining another request ID would exceed the configured bound.
    IdLimit,
}

/// An admitted action. The owner supplies real schemas, argument validation and
/// handlers for configured tools, and sends their eventual bounded responses.
#[derive(Debug, PartialEq)]
pub enum Action<T = ReadTool> {
    /// Send this lifecycle or protocol-error response.
    Respond(Value),
    /// List only these configured implementations with their real schemas.
    ListTools {
        /// Original request ID.
        id: Value,
        /// Implemented tools in stable order.
        tools: Vec<T>,
    },
    /// Validate arguments and invoke exactly this configured read implementation.
    Call {
        /// Original request ID.
        id: Value,
        /// Closed read identity.
        tool: T,
        /// Untrusted arguments; admission is not domain validation.
        arguments: Map<String, Value>,
    },
    /// No response or read dispatch is permitted for this notification.
    Ignore,
    /// Close the connection; the admission state is permanently closed.
    Close(CloseReason),
}

#[derive(PartialEq, Eq)]
enum Phase {
    New,
    AwaitingInitialized,
    Ready,
    Closed(CloseReason),
}

/// Session-local lifecycle and request-ID admission. Input must first pass
/// bounded framing; this structure retains at most `max_ids` bounded IDs.
pub struct ProtocolAdmission<T: ToolIdentity> {
    phase: Phase,
    implemented: BTreeSet<T>,
    ids: BTreeSet<String>,
    max_ids: NonZeroUsize,
}

/// Backward-compatible read-only lifecycle; it cannot configure task tools.
pub type Admission = ProtocolAdmission<ReadTool>;

impl<T: ToolIdentity> ProtocolAdmission<T> {
    /// Configure only tools with real implementations in the owning adapter.
    pub fn new(implemented: impl IntoIterator<Item = T>, max_ids: NonZeroUsize) -> Self {
        Self {
            phase: Phase::New,
            implemented: implemented.into_iter().collect(),
            ids: BTreeSet::new(),
            max_ids,
        }
    }

    /// Admit one framed value. No method name can become an arbitrary dispatch.
    pub fn receive(&mut self, message: Value) -> Action<T> {
        if let Phase::Closed(reason) = self.phase {
            return Action::Close(reason);
        }
        let Some(object) = message.as_object() else {
            return error(None, -32600, "Invalid request");
        };
        let id = object.get("id");
        let envelope_valid = object.get("jsonrpc") == Some(&json!("2.0"))
            && object.get("method").is_some_and(Value::is_string)
            && !object.contains_key("result")
            && !object.contains_key("error");
        let params_valid = object.get("params").is_none_or(Value::is_object);
        // Even malformed notification-like objects must never elicit a response.
        let Some(id) = id else {
            if envelope_valid
                && params_valid
                && object["method"] == "notifications/initialized"
                && self.phase == Phase::AwaitingInitialized
            {
                self.phase = Phase::Ready;
            }
            return Action::Ignore;
        };
        if !(id.is_string() || id.is_i64() || id.is_u64()) {
            return error(None, -32600, "Invalid request ID");
        }
        let id = id.clone();
        let key = id.to_string();
        if self.ids.contains(&key) {
            self.phase = Phase::Closed(CloseReason::ReusedId);
            return Action::Close(CloseReason::ReusedId);
        }
        if self.ids.len() == self.max_ids.get() {
            self.phase = Phase::Closed(CloseReason::IdLimit);
            return Action::Close(CloseReason::IdLimit);
        }
        self.ids.insert(key);
        if !envelope_valid {
            return error(Some(id), -32600, "Invalid request");
        }
        if !params_valid {
            return error(Some(id), -32602, "Invalid params");
        }
        let empty = Map::new();
        let params = object
            .get("params")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        match object["method"].as_str().unwrap_or_default() {
            "ping" => response(id, json!({})),
            "initialize" if self.phase == Phase::New => self.initialize(id, params),
            "initialize" => error(Some(id), -32600, "Already initialized"),
            "tools/list" | "tools/call" if self.phase != Phase::Ready => {
                error(Some(id), -32600, "Initialization incomplete")
            }
            "tools/list" if params.contains_key("cursor") => {
                error(Some(id), -32602, "Pagination unsupported")
            }
            "tools/call" if params.contains_key("task") => {
                error(Some(id), -32602, "Task augmentation unsupported")
            }
            "tools/list" if !self.implemented.is_empty() => Action::ListTools {
                id,
                tools: self.implemented.iter().copied().collect(),
            },
            "tools/call" if !self.implemented.is_empty() => {
                let tool = params.get("name").and_then(Value::as_str).and_then(|name| {
                    self.implemented
                        .iter()
                        .copied()
                        .find(|tool| tool.name() == name)
                });
                let arguments = params.get("arguments");
                if let Some(tool) = tool.filter(|_| arguments.is_none_or(Value::is_object)) {
                    Action::Call {
                        id,
                        tool,
                        arguments: arguments
                            .and_then(Value::as_object)
                            .cloned()
                            .unwrap_or_default(),
                    }
                } else {
                    error(Some(id), -32602, "Unknown tool or invalid arguments")
                }
            }
            _ => error(Some(id), -32601, "Method not found"),
        }
    }

    fn initialize(&mut self, id: Value, params: &Map<String, Value>) -> Action<T> {
        let client = params.get("clientInfo").and_then(Value::as_object);
        if !params.get("protocolVersion").is_some_and(Value::is_string)
            || !params.get("capabilities").is_some_and(Value::is_object)
            || !client.is_some_and(|info| {
                info.get("name").is_some_and(Value::is_string)
                    && info.get("version").is_some_and(Value::is_string)
            })
        {
            return error(Some(id), -32602, "Invalid initialization params");
        }
        self.phase = Phase::AwaitingInitialized;
        let capabilities = if self.implemented.is_empty() {
            json!({})
        } else {
            json!({"tools": {}})
        };
        response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": capabilities,
                "serverInfo": {"name": "cartograph", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
    }
}

fn response<T>(id: Value, result: Value) -> Action<T> {
    Action::Respond(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn error<T>(id: Option<Value>, code: i32, message: &'static str) -> Action<T> {
    let mut response = json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}});
    if let Some(id) = id {
        response["id"] = id;
    }
    Action::Respond(response)
}

#[cfg(test)]
mod tests;
