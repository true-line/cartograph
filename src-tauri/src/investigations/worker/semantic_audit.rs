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
        let (audit, audit_limits) = match self
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
        let configured = self.auditor;
        let provider = configured.map_or(provider, |auditor| auditor.provider.as_ref());
        let auditor_descriptor = descriptor(provider, InvestigationProviderMode::Local)?;
        if provider.locality() != Locality::Local
            || (configured.is_some() && auditor_descriptor.model == self.detail.provider.model)
            || (configured.is_none() && auditor_descriptor != self.detail.provider)
            || input.ledger() != &context.ledger
        {
            return Err(HostError::InputChanged.into());
        }
        let action = audit.action(format!("{}:audit-1", self.id))?;
        let prepared = firewall(self.state)?
            .prepare_bounded(provider, &action, &audit_limits)
            .map_err(provider_failure)?;
        drop(action);
        let auditor_selection =
            configured.map(|auditor| crate::jobs::investigations::SeparateAuditor {
                profile: prepared.profile().clone(),
                reported_model_digest: auditor.reported_model_digest.clone(),
                analyst_reported_model_digest: auditor.analyst_reported_model_digest.clone(),
            });
        if let Some(selected) = &auditor_selection {
            selected.validate_against_model(&self.detail.provider.model)?;
        }
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
                    separate_auditor: auditor_selection.as_ref(),
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
        // The durable debit precedes the one call. Only bounded transport/output
        // failures are advisory; privacy, cancellation and ownership still stop.
        let deadline = self.clock.deadline()?;
        let directive = || execution_directive(self.state, self.execution, self.id);
        let completion = provider.complete_bounded(
            &authorized,
            &CompletionControl {
                deadline,
                check: &directive,
            },
        );
        let completion = match &completion {
            Ok(completion) => Ok(completion),
            Err(error) => Err(match error.code {
                llm::bounded::FailureCode::Transport | llm::bounded::FailureCode::HttpStatus => {
                    agents::investigation::semantic_audit::AuditFailure::Transport
                }
                // A request timeout can precede the task deadline; settlement
                // still refuses publication when the task clock is exhausted.
                llm::bounded::FailureCode::DeadlineExceeded => {
                    agents::investigation::semantic_audit::AuditFailure::Timeout
                }
                llm::bounded::FailureCode::ResponseTooLarge
                | llm::bounded::FailureCode::OutputTooLarge => {
                    agents::investigation::semantic_audit::AuditFailure::Length
                }
                llm::bounded::FailureCode::UnsupportedEncoding
                | llm::bounded::FailureCode::InvalidResponse
                | llm::bounded::FailureCode::IncompleteResponse
                | llm::bounded::FailureCode::OutputLimitReached
                | llm::bounded::FailureCode::Refused
                | llm::bounded::FailureCode::EmptyCompletion => {
                    agents::investigation::semantic_audit::AuditFailure::InvalidOutput
                }
                _ => return Err(provider_failure(*error)),
            }),
        };
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
                    completion,
                    active_milliseconds: self.clock.active_milliseconds(),
                },
            )?
        };
        super::super::emit_changed(self.app, &self.detail);
        Ok(true)
    }
}
