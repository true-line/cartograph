//! Durable specialist investigations (SPEC-10). The coordinator owns execution;
//! graph/source acquisition is short-lived and model payloads remain transient.
mod area;
pub(crate) mod commands;
pub(crate) mod completion_choices;
mod context;
#[cfg(unix)]
pub(crate) use context::{inventory_admission_workspace, preparation_workspace};
mod events;
mod evidence;
pub(crate) mod finding_curation_commands;
pub(crate) mod finding_review_commands;
mod history;
pub(crate) mod inspection;
pub(crate) mod mcp_dispatch;
pub(crate) mod provider_workspace;
#[cfg(any(unix, test))]
pub(crate) mod query_workspace;
mod recovery;
mod runtime;
pub(crate) mod section_drafts;
#[cfg(unix)]
pub(crate) mod source_candidate_workspace;
#[cfg(unix)]
pub(crate) mod source_preparation_workspace;
#[cfg(unix)]
pub(crate) mod source_retained_workspace;
pub(crate) mod tool_runtime;
mod worker;

pub(super) use events::emit_changed;
pub(crate) use recovery::recover;
pub(crate) use runtime::InvestigationRuntime;

use agents::investigation::{InvestigationInputLedger, InvestigationUsage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HostError {
    #[error("Investigation context changed during acquisition.")]
    InputChanged,
    #[error("Investigation evidence is unavailable; no source was substituted.")]
    SourceUnavailable,
    #[error("Investigation input or source metadata is invalid.")]
    InvalidInput,
    #[error("Investigation limit exhausted.")]
    LimitExceeded,
    #[error("Investigation durable store capacity exhausted.")]
    StoreCapacity,
    #[error("Investigation storage or source ownership could not be verified.")]
    Operational,
}

impl From<agents::investigation::InvestigationError> for HostError {
    fn from(error: agents::investigation::InvestigationError) -> Self {
        match error {
            agents::investigation::InvestigationError::LimitExceeded => Self::LimitExceeded,
            _ => Self::InvalidInput,
        }
    }
}

/// Implemented by the owning worker's durable transition adapter. Publication
/// happens while source guards still protect every newly supplied receipt.
trait AcquisitionSink {
    fn usage(&self) -> &InvestigationUsage;
    fn reserve_validation(&mut self, bytes: u64) -> Result<(), HostError>;
    fn publish_input(
        &mut self,
        ledger: &InvestigationInputLedger,
        usage: InvestigationUsage,
    ) -> Result<(), HostError>;
}
