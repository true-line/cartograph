//! Closed task envelopes after authentication and frame admission. This parser
//! neither grants authority nor validates a start's domain request. The host
//! must validate that request against its specialist/policy contract before use.
use crate::tools::TaskTool;
use serde_json::{Map, Value};

/// Parsed envelope, not an execution grant. No principal is accepted here.
#[derive(Debug, PartialEq)]
pub enum TaskArguments {
    /// Untrusted domain request, moved intact for host validation.
    Start {
        /// Original request fields; host policy/intent decoding is still required.
        request: Map<String, Value>,
    },
    /// Read an owned task's status.
    Status {
        /// Opaque durable ID; ownership is checked by the host.
        task_id: String,
    },
    /// Read events after a saved sequence; the host checks its journal bounds.
    Events {
        /// Opaque durable ID; ownership is checked by the host.
        task_id: String,
        /// Exclusive sequence cursor, still subject to host journal bounds.
        after: u64,
    },
    /// Read an owned saved result.
    Result {
        /// Opaque durable ID; ownership is checked by the host.
        task_id: String,
    },
    /// Request cancellation; no family, provider or consent choice is accepted.
    Cancel {
        /// Opaque durable ID; ownership is checked by the host.
        task_id: String,
    },
    /// Stage a finding as its agent's attested answer to a Gap; no verb,
    /// contributor or confirmation is accepted.
    ProposeAttested {
        /// Opaque durable ID; ownership is checked by the host.
        task_id: String,
        /// The finding of the task's saved result.
        finding_id: String,
        /// The Gap the finding answers; the host checks it is open.
        gap_id: String,
        /// The Gap's slot.
        slot: String,
    },
}

/// The byte bound of a finding, Gap or slot identifier.
pub const MAX_ATTESTED_REFERENCE_BYTES: usize = 512;

/// Invalid envelope; error text never includes caller input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid task arguments")]
pub struct InvalidTaskArguments;

impl TaskArguments {
    /// Consume an already frame-bounded argument map without cloning its tree.
    /// Call only after the host checks the live session grant. A valid envelope
    /// proves neither task ownership nor start-request validity.
    pub fn parse(
        tool: TaskTool,
        mut arguments: Map<String, Value>,
    ) -> Result<Self, InvalidTaskArguments> {
        if tool == TaskTool::Start {
            if arguments.len() != 1 {
                return Err(InvalidTaskArguments);
            }
            return match arguments.remove("request") {
                Some(Value::Object(request)) => Ok(Self::Start { request }),
                _ => Err(InvalidTaskArguments),
            };
        }
        let fields = match tool {
            TaskTool::Events => 2,
            TaskTool::ProposeAttested => 4,
            _ => 1,
        };
        if arguments.len() != fields {
            return Err(InvalidTaskArguments);
        }
        let Some(Value::String(task_id)) = arguments.remove("task_id") else {
            return Err(InvalidTaskArguments);
        };
        // Same opaque task-ID grammar as the durable host store, in UTF-8 bytes.
        if task_id.is_empty()
            || task_id.len() > 128
            || !task_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
        {
            return Err(InvalidTaskArguments);
        }
        Ok(match tool {
            TaskTool::Status => Self::Status { task_id },
            TaskTool::Result => Self::Result { task_id },
            TaskTool::Cancel => Self::Cancel { task_id },
            TaskTool::Events => {
                let after = arguments
                    .remove("after")
                    .and_then(|v| v.as_u64())
                    .ok_or(InvalidTaskArguments)?;
                Self::Events { task_id, after }
            }
            TaskTool::ProposeAttested => {
                let mut reference = |field| match arguments.remove(field) {
                    Some(Value::String(value))
                        if !value.is_empty()
                            && value.len() <= MAX_ATTESTED_REFERENCE_BYTES
                            && !value.chars().any(char::is_control) =>
                    {
                        Ok(value)
                    }
                    _ => Err(InvalidTaskArguments),
                };
                Self::ProposeAttested {
                    task_id,
                    finding_id: reference("finding_id")?,
                    gap_id: reference("gap_id")?,
                    slot: reference("slot")?,
                }
            }
            TaskTool::Start => unreachable!("start returned before task-id parsing"),
        })
    }
}

#[cfg(test)]
mod tests;
