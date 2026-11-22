//! Publish the real guarded metadata preparation only after durable admission.
use super::*;
use crate::investigations::runtime::RunClock;
use crate::job_execution::JobExecution;
use crate::jobs::investigations::{InvestigationStoreError, MetadataAdmission};

#[derive(Debug, thiserror::Error)]
pub(in crate::investigations) enum MetadataPublicationError {
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Store(#[from] InvestigationStoreError),
}

impl FrozenContext {
    /// The database writer authenticates the saved call and continuation. This
    /// adapter supplies the actual preparation and retains its guards through
    /// COMMIT. No prepared bytes enter the running context on an error, including
    /// an uncertain commit acknowledgement. The caller must stop on that error.
    pub(in crate::investigations) fn publish_admitted_metadata(
        &mut self,
        state: &AppState,
        execution: &JobExecution,
        expected_revision: u64,
        clock: &RunClock,
        prepared: PreparedContextRead,
        admission: &MetadataAdmission<'_>,
    ) -> Result<InvestigationDetail, MetadataPublicationError> {
        self.validate_prepared_context(&prepared)?;
        let kind = match prepared.kind() {
            PreparedContextReadKind::Query => ContextPreviewKind::Query,
            PreparedContextReadKind::Inventory => ContextPreviewKind::Inventory,
        };
        if self.policy.profile() != INVESTIGATION_V2_POLICY.profile()
            || (prepared.kind() == PreparedContextReadKind::Query) != prepared.guards.is_some()
            || admission.kind != kind
            || admission.current.ledger() != prepared.base_ledger()
            || admission.candidate.ledger() != prepared.ledger()
            || admission.active_milliseconds > clock.active_milliseconds()
        {
            return Err(HostError::InputChanged.into());
        }
        // Every fallible allocation precedes publication. The guarded candidate
        // is moved, not reconstructed from a newer graph or registry snapshot.
        match prepared.kind() {
            PreparedContextReadKind::Query => self.query_pages.try_reserve(1),
            PreparedContextReadKind::Inventory => self.inventory_details.try_reserve(1),
        }
        .map_err(|_| HostError::LimitExceeded)?;
        clock.deadline()?;
        let mut retained_error = None;
        let result = state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .admit_next_prompt_metadata(
                execution,
                expected_revision,
                admission,
                prepared.raw(),
                |view| {
                    let checked = clock.deadline().map(|_| ()).and_then(|()| {
                        if let Some(guards) = prepared.guards.as_ref() {
                            guards.revalidate_in_view(&state.primary_sources, view)
                        } else {
                            Ok(())
                        }
                    });
                    if let Err(error) = checked {
                        retained_error = Some(error);
                        return Err(InvestigationStoreError::Invalid);
                    }
                    Ok(())
                },
            );
        if let Some(error) = retained_error {
            return Err(error.into());
        }
        let detail = result?;
        let PreparedContextRead {
            ledger,
            raw,
            kind,
            guards,
            rows,
            ..
        } = prepared;
        self.ledger = ledger;
        self.retain_rows(rows);
        match kind {
            PreparedContextReadKind::Query => self.query_pages.push(raw),
            PreparedContextReadKind::Inventory => self.inventory_details.push(raw),
        }
        drop(guards);
        Ok(detail)
    }
}
