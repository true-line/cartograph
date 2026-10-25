//! Closed, read-only investigation protocol (SPEC-10, ADR-0028).
//!
//! The host owns graph access, receipts, execution and durable transitions. This
//! module accepts bounded input data and admits model actions; it has no graph
//! store, filesystem, network dispatch or target-code mutation authority.

mod admission;
mod completion;
mod coverage;
mod coverage_protocol;
mod disclosure;
mod disclosure_workspace;
mod finding_review;
mod input;
mod inspection;
mod inventory_workspace;
mod mcp_client_id;
mod mcp_grant;
// Separate completion families preserve every legacy generation and identity.
#[doc(hidden)]
pub mod obligations;
mod policy;
mod privacy;
mod projection;
mod protocol;
mod record_workspace;
mod records;
mod result_workspace;
mod sealing;
mod specialists;
#[doc(hidden)]
pub mod staged_attempt;
#[doc(hidden)]
pub mod staged_finish;
#[doc(hidden)]
pub mod staged_refusal;
mod strict;
mod task_validation_workspace;
mod turn;
mod validation_workspace;

pub use completion::*;
pub use coverage::*;
pub use coverage_protocol::*;
pub use disclosure::*;
pub use disclosure_workspace::disclosure_validation_workspace;
pub use finding_review::*;
pub use input::*;
pub use inspection::*;
pub use inventory_workspace::{InventoryPreparationWorkspace, inventory_preparation_workspace};
pub use mcp_client_id::McpClientId;
pub use mcp_grant::{McpGrantSnapshot, McpGrantUnavailable, McpTaskActivation, McpTaskSession};
pub use obligations::*;
pub use policy::{
    INVESTIGATION_V1_POLICY, INVESTIGATION_V2_POLICY, InvestigationPolicy,
    MAX_STORED_COMPLETION_ATTEMPTS,
};
pub use privacy::{redacted_history_text, reject_prose_replay};
pub use projection::*;
pub use protocol::*;
pub use record_workspace::record_decode_scratch_bytes;
pub use records::*;
pub use result_workspace::completion_result_validation_workspace;
pub use specialists::*;
pub use task_validation_workspace::next_prompt_validation_workspace;
pub use turn::*;
pub use validation_workspace::{
    attempt_binding_validation_workspace, coverage_archive_validation_workspace,
    ledger_validation_workspace,
};

#[cfg(test)]
mod tests;

/// Hard protocol and storage boundaries. Host counters may narrow these limits.
pub const MAX_ACTION_BYTES: usize = 32 * 1024;
pub const MAX_INPUT_BYTES: usize = 128 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 128 * 1024;
pub const MAX_RESULT_BYTES: usize = 64 * 1024;
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
/// Shape limits shared with logical admission models; values are unchanged.
pub const RECORD_MAX_DEPTH: usize = 32;
pub const RECORD_MAX_VALUES: usize = 16384;
pub const RECORD_MAX_ARRAY_ITEMS: usize = 128;
/// Cumulative raw query acquisitions, including repeated identical pages.
pub const MAX_TOTAL_QUERY_BYTES: usize = 96 * 1024;
pub const MAX_QUERY_BYTES: usize = 32 * 1024;
pub const MAX_QUERY_FACTS: usize = 32;
pub const MAX_SELECTED_FACTS: usize = 64;
pub const MAX_QUESTION_BYTES: usize = 2048;
pub const MAX_MODEL_CALLS: u32 = 8;
pub const MAX_TOOL_ACTIONS: u32 = 8;

/// Fixed diagnostics never expose source, prompts or unadmitted model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvestigationError {
    #[error("invalid investigation request")]
    InvalidRequest,
    #[error("invalid investigation action")]
    InvalidAction,
    #[error("investigation bound exceeded")]
    LimitExceeded,
    #[error("invalid investigation input")]
    InvalidInput,
    #[error("investigation output cites unavailable input")]
    InvalidCitation,
    #[error("investigation output repeats supplied text")]
    SourceReplay,
    #[error("investigation output contains sensitive text")]
    SensitiveOutput,
}

pub(crate) fn bounded_bytes(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<Vec<u8>, InvestigationError> {
    crate::source_basis::bounded_json(value, limit).map_err(|_| InvestigationError::LimitExceeded)
}

pub(crate) fn text(value: &str, maximum: usize) -> bool {
    crate::source_basis::text(value, maximum)
}

/// Versioned, bounded decoding for durable typed investigation bodies. SQL
/// callers must additionally preflight storage type and UTF-8 byte length before
/// retrieving a body; this method cannot retroactively bound that allocation.
pub fn decode_record<T: serde::de::DeserializeOwned + serde::Serialize>(
    body: &str,
) -> Result<T, InvestigationError> {
    strict::preflight(
        body,
        MAX_RECORD_BYTES,
        RECORD_MAX_DEPTH,
        RECORD_MAX_VALUES,
        RECORD_MAX_ARRAY_ITEMS,
    )?;
    let value: T = serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
    // New investigation contracts have no legacy permissive fields. Typed
    // round-trip equality also closes nested DTOs reused from older contracts.
    // Typed serialization can expand a small input. Enforce the existing byte
    // ceiling before constructing either comparison Value.
    let canonical = bounded_bytes(&value, MAX_RECORD_BYTES)?;
    let original: serde_json::Value =
        serde_json::from_str(body).map_err(|_| InvestigationError::InvalidInput)?;
    let round_trip: serde_json::Value =
        serde_json::from_slice(&canonical).map_err(|_| InvestigationError::InvalidInput)?;
    if round_trip != original {
        return Err(InvestigationError::InvalidInput);
    }
    Ok(value)
}

#[cfg(test)]
mod result_disclosure_tests;

#[cfg(test)]
mod expanded_capacity_witness_tests;

mod query_history;
pub use query_history::*;

mod query_history_protocol;
pub use query_history_protocol::*;
