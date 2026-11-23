//! Closed protocol vocabulary. Listing a tool is never an execution grant;
//! the host registers only handlers available to its authenticated session.
use crate::admission::ReadTool;

mod sealed {
    pub trait Sealed {}
}

/// Closed identities accepted by the shared protocol lifecycle.
pub trait ToolIdentity: sealed::Sealed + Copy + Ord {
    /// Stable external tool name.
    fn name(self) -> &'static str;
}
impl sealed::Sealed for ReadTool {}
impl ToolIdentity for ReadTool {
    fn name(self) -> &'static str {
        ReadTool::name(self)
    }
}

/// Durable Cartograph task tools, distinct from MCP task augmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskTool {
    /// Admit or recover a durable investigation request.
    Start,
    /// Read owned task status.
    Status,
    /// Read an ordered, bounded event page.
    Events,
    /// Read the owned saved result.
    Result,
    /// Request durable cancellation.
    Cancel,
    /// Stage a finding of an owned completed task as its agent's attested
    /// answer to a Gap, for review in the app (ADR-0090 decision 9). It can
    /// never confirm.
    ProposeAttested,
}
impl TaskTool {
    /// Stable external name; no arbitrary host method dispatch.
    pub fn name(self) -> &'static str {
        match self {
            Self::Start => "cartograph_task_start",
            Self::Status => "cartograph_task_status",
            Self::Events => "cartograph_task_events",
            Self::Result => "cartograph_task_result",
            Self::Cancel => "cartograph_task_cancel",
            Self::ProposeAttested => "cartograph_task_propose_attested",
        }
    }
}

/// Shared tool set for one lifecycle and one request-ID ledger per connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    /// Existing recovered-context read.
    Read(ReadTool),
    /// Owner-granted durable investigation operation.
    Task(TaskTool),
}
impl sealed::Sealed for Tool {}
impl ToolIdentity for Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Read(tool) => tool.name(),
            Self::Task(tool) => tool.name(),
        }
    }
}
