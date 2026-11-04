//! One audit on the original task account and RunClock; never a child task.
use super::*;
use crate::investigations::evidence::EvidenceGuards;
use crate::jobs::investigations::{SemanticAuditResponse, SemanticAuditStart};

impl<R: tauri::Runtime> Worker<'_, R> {
    pub(super) fn dispatch_semantic_audit(
        &mut self,
        provider: &dyn LlmProvider,
        input: &InvestigationInput,
        original: &PreparedBoundedCompletion,
        context: &FrozenContext,
    ) -> Result<bool, Failure> {
        self.check_new_work()?;
        let audit = match self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .prepare_next_prompt_semantic_audit(
                self.execution,
                self.detail.summary.revision,
                input,
                original,
            ) {
            Ok(audit) => audit,
            Err(InvestigationStoreError::Stale) => return Ok(false), // earlier analyst refusal
            Err(error) => return Err(error.into()),
        };
        if provider.locality() != Locality::Local
            || descriptor(provider, InvestigationProviderMode::Local)? != self.detail.provider
            || input.ledger() != &context.ledger
        {
            return Err(HostError::InputChanged.into());
        }
        let action = audit.action(format!("{}:audit-1", self.id))?;
        let prepared = firewall(self.state)?
            .prepare_bounded(provider, &action, original.limits())
            .map_err(provider_failure)?;
        drop(action);
        let authorized = firewall(self.state)?
            .authorize_bounded(provider, &prepared, None)
            .map_err(provider_failure)?;
        self.check_new_work()?;
        let keys = context
            .ledger
            .selected_facts
            .iter()
            .map(|selected| crate::task_evidence::graph_fact(&selected.fact))
            .collect::<Vec<_>>();
        let selected = context.select(self.state, &keys)?;
        let guards = EvidenceGuards::acquire(self.state, &selected)?;
        let mut retained_error = None;
        let started = self
            .state
            .jobs
            .lock()
            .map_err(|_| HostError::Operational)?
            .begin_next_prompt_semantic_audit(
                self.execution,
                self.detail.summary.revision,
                &SemanticAuditStart {
                    input,
                    original_prepared: original,
                    prepared: &prepared,
                    authorized: &authorized,
                    active_milliseconds: self.clock.active_milliseconds(),
                },
                |view| {
                    let checked = self.clock.deadline().map(|_| ()).and_then(|()| {
                        guards.revalidate_in_view(&self.state.primary_sources, view)
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
        self.detail = started?;
        drop(guards);
        // Source validation owners belong to begin's workspace, not transport.
        drop(selected);
        drop(keys);
        super::super::emit_changed(self.app, &self.detail);
        // The durable debit precedes the call. Every error returns through the
        // ordinary worker stop path, retaining the audit charge and clock sample.
        let deadline = self.clock.deadline()?;
        let directive = || execution_directive(self.state, self.execution, self.id);
        let completion = provider
            .complete_bounded(
                &authorized,
                &CompletionControl {
                    deadline,
                    check: &directive,
                },
            )
            .map_err(provider_failure)?;
        validate_completion(&completion, &prepared)?;
        self.detail = {
            let mut jobs = self.state.jobs.lock().map_err(|_| HostError::Operational)?;
            // Cancellation may increment revision while the bounded provider is
            // in flight. Retain a completed response as a cancelled receipt,
            // rather than treating that known response as an unknown outcome.
            // The settlement still checks owner, phase and exact invocation.
            let current = jobs.investigation_owned_detail(self.execution, self.id)?;
            jobs.settle_next_prompt_semantic_audit(
                self.execution,
                current.summary.revision,
                &SemanticAuditResponse {
                    input,
                    original_prepared: original,
                    audit_prepared: &prepared,
                    completion: &completion,
                    active_milliseconds: self.clock.active_milliseconds(),
                },
            )?
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }
}
