//! Authenticated transport adapter over the existing durable coordinator.
//! No provider configuration, principal or consent decision comes from the wire.
// Its only caller is the Unix-only MCP service (#227).
#![cfg_attr(not(unix), allow(dead_code))]
use crate::mcp_reads::{ReadError, scope::ScopeLimits, task_reservation::TaskReservations};
use agents::investigation::McpTaskSession;
use mcp_wire::{task_arguments::TaskArguments, tools::TaskTool};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tauri::Manager;

/// Host-admitted limits retained by detached task execution; never wire input.
#[derive(Clone, Copy)]
pub(crate) struct TaskReadLimits {
    pub scope: ScopeLimits,
    pub evidence: crate::mcp_reads::dispatch::EvidenceWorkLimits,
}

pub(crate) const TOOLS: [TaskTool; 6] = [
    TaskTool::Start,
    TaskTool::Status,
    TaskTool::Events,
    TaskTool::Result,
    TaskTool::Cancel,
    TaskTool::ProposeAttested,
];

pub(crate) struct TaskDispatcher {
    session: McpTaskSession,
    reservations: TaskReservations,
}

impl TaskDispatcher {
    pub(crate) fn new(session: McpTaskSession, reservations: TaskReservations) -> Self {
        Self {
            session,
            reservations,
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.session.with_grant(|_, _| ()).is_ok()
    }

    pub(crate) fn call<R: tauri::Runtime>(
        &self,
        app: &tauri::AppHandle<R>,
        id: &Value,
        tool: TaskTool,
        arguments: Map<String, Value>,
        limits: TaskReadLimits,
        cap: usize,
    ) -> Result<Value, ReadError> {
        // Refuse before decoding a domain request or touching storage. Each
        // durable operation independently holds the same grant during access.
        if !self.available() {
            return Err(ReadError::InvalidRequest);
        }
        let request =
            TaskArguments::parse(tool, arguments).map_err(|_| ReadError::InvalidRequest)?;
        let state = app.state::<crate::AppState>();
        match request {
            TaskArguments::Start { request } => {
                let request = serde_json::from_value(Value::Object(request))
                    .map_err(|_| ReadError::InvalidRequest)?;
                let summary = super::commands::start_mcp(
                    request,
                    app.clone(),
                    &self.session,
                    limits,
                    &self.reservations,
                )
                .map_err(|error| match error {
                    super::commands::InvestigationStartError::Rejected {
                        code: "limit_exceeded",
                        ..
                    } => ReadError::AdmissionLimit,
                    super::commands::InvestigationStartError::Rejected {
                        code: "refresh_required",
                        ..
                    } => ReadError::RefreshRequired,
                    super::commands::InvestigationStartError::Rejected { .. } => {
                        ReadError::InvalidRequest
                    }
                    super::commands::InvestigationStartError::Legacy(_) => ReadError::Storage,
                })?;
                response(id, &summary, cap)
            }
            TaskArguments::Status { task_id } => {
                let detail = state
                    .jobs
                    .lock()
                    .map_err(|_| ReadError::Storage)?
                    .investigation_access(&task_id, Some(&self.session))
                    .map_err(|_| ReadError::InvalidRequest)?;
                response(id, &detail.summary, cap)
            }
            TaskArguments::Events { task_id, after } => {
                let page = state
                    .jobs
                    .lock()
                    .map_err(|_| ReadError::Storage)?
                    .investigation_events_access(&task_id, after, Some(&self.session))
                    .map_err(|_| ReadError::InvalidRequest)?;
                response(id, &page, cap)
            }
            TaskArguments::Result { task_id } => {
                let result = state
                    .jobs
                    .lock()
                    .map_err(|_| ReadError::Storage)?
                    .investigation_result_access(&task_id, Some(&self.session))
                    .map_err(|_| ReadError::InvalidRequest)?;
                // MCP structuredContent remains an object even before a saved
                // result exists; pending and ready use the same envelope.
                #[derive(Serialize)]
                struct ResultReply<'a, T> {
                    result: &'a Option<T>,
                    /// Present for a drafting run only (ADR-0066): whether
                    /// the run staged a draft for review, by closed code.
                    #[serde(skip_serializing_if = "Option::is_none")]
                    drafting: Option<super::section_drafts::DraftingOutcome>,
                }
                // The read above admitted this session to the task. Staging
                // is idempotent and takes the jobs lock itself.
                let drafting = super::section_drafts::outcome_of_drafting_run(&state, &task_id)
                    .map_err(|_| ReadError::Storage)?;
                response(
                    id,
                    &ResultReply {
                        result: &result,
                        drafting,
                    },
                    cap,
                )
            }
            TaskArguments::Cancel { task_id } => {
                let detail = state
                    .jobs
                    .lock()
                    .map_err(|_| ReadError::Storage)?
                    .cancel_investigation_access(&task_id, false, Some(&self.session))
                    .map_err(|_| ReadError::InvalidRequest)?;
                state.investigations.wake(&task_id);
                super::emit_changed(app, &detail);
                response(id, &detail.summary, cap)
            }
            TaskArguments::ProposeAttested {
                task_id,
                finding_id,
                gap_id,
                slot,
            } => self
                .session
                // The grant is held across the whole staging, so a
                // revocation either refuses it or waits for it (ADR-0073).
                .with_grant(|client, _| {
                    // Admit this session to the task before anything is read
                    // or staged; a task it does not own reads as unknown.
                    state
                        .jobs
                        .lock()
                        .map_err(|_| ReadError::Storage)?
                        .require_mcp_owner(&task_id, client)
                        .map_err(|_| ReadError::InvalidRequest)?;
                    // Staged for review in the app, never directed by or
                    // confirmed for the engineer (ADR-0090 decision 9).
                    let stored = crate::attested_capture::propose_from_finding(
                        &state,
                        &task_id,
                        &finding_id,
                        &gap_id,
                        &slot,
                        false,
                    )
                    .map_err(|_| ReadError::InvalidRequest)?;
                    response(id, &ProposedReply::from(&stored), cap)
                })
                .map_err(|_| ReadError::InvalidRequest)?,
        }
    }
}

/// What a staged proposal returns: its id and standing, never its
/// contributors.
#[derive(Serialize)]
struct ProposedReply<'a> {
    record_id: &'a str,
    citation_id: &'a str,
    standing: agents::attested::Standing,
    attestation_count: u64,
}

impl<'a> From<&'a agents::attested_store::StoredAttested> for ProposedReply<'a> {
    fn from(stored: &'a agents::attested_store::StoredAttested) -> Self {
        Self {
            record_id: stored.record_id.as_str(),
            citation_id: stored.citation_id.as_str(),
            standing: stored.standing,
            attestation_count: stored.attestation_count(),
        }
    }
}

fn response(id: &Value, payload: &impl Serialize, cap: usize) -> Result<Value, ReadError> {
    #[derive(Serialize)]
    struct ResultBody<'a, T> {
        content: [Text; 1],
        #[serde(rename = "structuredContent")]
        structured_content: &'a T,
        #[serde(rename = "isError")]
        is_error: bool,
    }
    #[derive(Serialize)]
    struct Text {
        #[serde(rename = "type")]
        kind: &'static str,
        text: &'static str,
    }
    #[derive(Serialize)]
    struct Reply<'a, T> {
        jsonrpc: &'static str,
        id: &'a Value,
        result: ResultBody<'a, T>,
    }
    let reply = Reply {
        jsonrpc: "2.0",
        id,
        result: ResultBody {
            content: [Text {
                kind: "text",
                text: "Task response; consume structuredContent.",
            }],
            structured_content: payload,
            is_error: false,
        },
    };
    // Serialize the borrowed DTO with the full envelope limit before allocating
    // a JSON tree. The bounded bytes and resulting tree overlap explicitly.
    let bytes = super::context::bounded_json(&reply, cap).map_err(|_| ReadError::Limit)?;
    serde_json::from_slice(&bytes).map_err(|_| ReadError::Storage)
}

pub(crate) fn descriptor(tool: TaskTool) -> Value {
    let schema = match tool {
        TaskTool::Start => {
            json!({"type":"object","properties":{"request":{"type":"object"}},"required":["request"],"additionalProperties":false})
        }
        TaskTool::Events => {
            json!({"type":"object","properties":{"task_id":{"type":"string"},"after":{"type":"integer","minimum":0}},"required":["task_id","after"],"additionalProperties":false})
        }
        TaskTool::ProposeAttested => {
            // The parser bounds UTF-8 bytes, which no JSON Schema keyword
            // states, so the bound is described rather than advertised as
            // a character count that would be too wide or too narrow.
            let reference = json!({"type":"string","minLength":1,"description":format!("At most {} UTF-8 bytes, no control characters.", mcp_wire::task_arguments::MAX_ATTESTED_REFERENCE_BYTES)});
            json!({"type":"object","properties":{"task_id":{"type":"string"},"finding_id":reference,"gap_id":reference,"slot":reference},"required":["task_id","finding_id","gap_id","slot"],"additionalProperties":false})
        }
        _ => {
            json!({"type":"object","properties":{"task_id":{"type":"string"}},"required":["task_id"],"additionalProperties":false})
        }
    };
    json!({"name":tool.name(),"description":"Owner-granted durable investigation operation.","inputSchema":schema})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_response_bounds_complete_envelope_before_value_decode() {
        // AC-0327: id escaping and the MCP wrapper spend the same byte cap.
        let id = json!("escaped\"λ");
        let payload = json!({"result":"quoted\"λ"});
        let value = response(&id, &payload, 4096).unwrap();
        let exact = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(response(&id, &payload, exact).unwrap(), value);
        assert_eq!(response(&id, &payload, exact - 1), Err(ReadError::Limit));
        assert_eq!(response(&id, &"x".repeat(4096), 256), Err(ReadError::Limit));
        assert_eq!(value["result"]["structuredContent"], payload);
    }
}
