//! Closed, read-only investigation protocol (SPEC-10, ADR-0028).
//!
//! The host owns graph access, receipts, execution and durable transitions. This
//! module accepts bounded input data and admits model actions; it has no graph
//! store, filesystem, network dispatch or target-code mutation authority.

#[doc(hidden)]
pub mod acquisition_choices;
mod admission;
pub mod admitted_action;
mod capabilities;
pub mod claim_diagnostics;
mod completion;
mod coverage;
mod coverage_protocol;
pub mod diagnostic_sidecar;
mod disclosure;
mod disclosure_workspace;
#[path = "enumeration_claims/guard.rs"]
mod enumeration_claims;
mod finding_review;
#[doc(hidden)]
pub mod finish_patch;
pub mod native_tools;
mod input;
mod inspection;
mod inventory_workspace;
mod mcp_client_id;
mod mcp_grant;
pub mod rejected_response;
// Separate completion families preserve every legacy generation and identity.
#[doc(hidden)]
pub mod obligations;
mod policy;
mod privacy;
mod projection;
mod protocol;
#[doc(hidden)]
pub mod query_refusal;
mod record_workspace;
mod records;
mod replay_location;
mod response_schema;
mod result_workspace;
mod sealing;
#[doc(hidden)]
pub mod semantic_audit;
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
mod withholding;
mod withholding_witness;

pub use capabilities::*;
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
    LEGACY_SUPPLIED_HISTORY_BYTES, MAX_STORED_COMPLETION_ATTEMPTS, PARENT_HISTORY_32768_V1_BYTES,
};
pub use privacy::{redacted_history_text, reject_prose_replay};
pub use projection::*;
pub use protocol::*;
pub use record_workspace::record_decode_scratch_bytes;
pub use records::*;
pub use replay_location::{
    ProseField, ProsePosition, ReplayLocation, locate_prose_replay,
    replay_location_workspace_bytes, replay_locations_workspace_bytes, visit_prose_replays,
    visit_prose_replays_with_metadata,
};
pub use result_workspace::{
    completion_result_validation_workspace, stored_refusal_validation_workspace,
};
pub use specialists::*;
pub use task_validation_workspace::next_prompt_validation_workspace;
pub use turn::*;
pub use validation_workspace::{
    attempt_binding_validation_workspace, coverage_archive_validation_workspace,
    ledger_validation_workspace,
};
pub use withholding::{FindingWithholdingPlan, WithheldFinishProjection};
pub use withholding_witness::{
    FINDINGS_WITHHELD_MESSAGE, FindingWithholdingWitness, WithholdingDisposition,
};

#[cfg(test)]
mod tests;

// Hard protocol and storage boundaries. Host counters may narrow these limits.

/// Largest accepted model action, in bytes.
pub const MAX_ACTION_BYTES: usize = 32 * 1024;
/// Largest assembled investigation input, in bytes.
pub const MAX_INPUT_BYTES: usize = 128 * 1024;
/// Largest input manifest, in bytes.
pub const MAX_MANIFEST_BYTES: usize = 128 * 1024;
/// Largest admitted investigation result, in bytes.
pub const MAX_RESULT_BYTES: usize = 64 * 1024;
/// Largest stored investigation record, in bytes.
pub const MAX_RECORD_BYTES: usize = 256 * 1024;
// Shape limits shared with logical admission models; values are unchanged.

/// Deepest JSON nesting accepted in a record.
pub const RECORD_MAX_DEPTH: usize = 32;
/// Most JSON values accepted in a record.
pub const RECORD_MAX_VALUES: usize = 16384;
/// Most items accepted in one record array.
pub const RECORD_MAX_ARRAY_ITEMS: usize = 128;
/// Cumulative raw query acquisitions, including repeated identical pages.
pub const MAX_TOTAL_QUERY_BYTES: usize = 96 * 1024;
/// Largest single raw query acquisition, in bytes.
pub const MAX_QUERY_BYTES: usize = 32 * 1024;
/// Most facts one query may return.
pub const MAX_QUERY_FACTS: usize = 32;
/// Most facts an investigation may select.
pub const MAX_SELECTED_FACTS: usize = 64;
/// Largest investigation question, in bytes.
pub const MAX_QUESTION_BYTES: usize = 2048;
/// Most model calls in one investigation.
pub const MAX_MODEL_CALLS: u32 = 8;
/// Most tool actions in one investigation.
pub const MAX_TOOL_ACTIONS: u32 = 8;

/// Fixed diagnostics never expose source, prompts or unadmitted model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvestigationError {
    /// The request is malformed or out of bounds.
    #[error("invalid investigation request")]
    InvalidRequest,
    /// A model action is malformed or not permitted.
    #[error("invalid investigation action")]
    InvalidAction,
    /// A hard size, count or call bound was exceeded.
    #[error("investigation bound exceeded")]
    LimitExceeded,
    /// The assembled input is malformed.
    #[error("invalid investigation input")]
    InvalidInput,
    /// The output cites input that was not supplied.
    #[error("investigation output cites unavailable input")]
    InvalidCitation,
    /// The output replays supplied source text.
    #[error("investigation output repeats supplied text")]
    SourceReplay,
    /// The output contains text the redactor flags as sensitive.
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
