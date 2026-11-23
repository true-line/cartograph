//! Durable specialist investigations (SPEC-10). The coordinator owns execution;
//! graph/source acquisition is short-lived and model payloads remain transient.
mod area;
pub(crate) mod artifact_reads_view;
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
use app_store::investigation_runtime as runtime;
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

pub(crate) use app_store::investigation_host::HostError;

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
